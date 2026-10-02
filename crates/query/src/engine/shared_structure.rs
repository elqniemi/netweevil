//! Reuse of identical, immutable structures between engines.
//!
//! Profiles on one dataset often agree on which edges are traversable, so
//! their routing graphs have bit-identical adjacency arrays, the same
//! snapping index and the same ranked hierarchy topology. Engines intern
//! those structures here: a new array is replaced by an equal live one, and
//! entries are weak, so nothing outlives the engines that use it. Interning
//! compares full contents (bitwise for floats), so sharing never changes a
//! value.

use std::sync::{Arc, Mutex, PoisonError, Weak};

use netweevil_core::TopologyBundle;

use crate::edge_spatial_index::{EdgeSpatialIndex, routable_edge_set};

/// Live interned arrays of one element type.
pub(crate) struct ArrayInterner<T: 'static> {
    entries: Mutex<Vec<Weak<[T]>>>,
}

impl<T: bytemuck::Pod> ArrayInterner<T> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// `values` as a shared array: an existing live array with the same
    /// bytes, or a new one that later equal arrays will share.
    pub(crate) fn intern(&self, values: Vec<T>) -> Arc<[T]> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|entry| entry.strong_count() > 0);
        let bytes = bytemuck::cast_slice::<T, u8>(&values);
        if let Some(existing) = entries.iter().filter_map(Weak::upgrade).find(|existing| {
            existing.len() == values.len() && bytemuck::cast_slice::<T, u8>(existing) == bytes
        }) {
            return existing;
        }
        let shared: Arc<[T]> = values.into();
        entries.push(Arc::downgrade(&shared));
        shared
    }
}

pub(crate) static U32_ARRAYS: ArrayInterner<u32> = ArrayInterner::new();
pub(crate) static F64_ARRAYS: ArrayInterner<f64> = ArrayInterner::new();

/// Structures derived from one shared object, live while any user holds
/// them.
pub(crate) struct DerivedCache<K: 'static, V: 'static> {
    entries: Mutex<Vec<(Weak<K>, Weak<V>)>>,
}

impl<K, V> DerivedCache<K, V> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// The live value derived from `key`, or `build()`, which later callers
    /// with the same `key` then share. Concurrent first calls may both
    /// build; either result is equal.
    pub(crate) fn get_or_build(&self, key: &Arc<K>, build: impl FnOnce() -> V) -> Arc<V> {
        let lookup = || {
            let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries.retain(|(known, value)| known.strong_count() > 0 && value.strong_count() > 0);
            entries
                .iter()
                .find(|(known, _)| std::ptr::eq(known.as_ptr(), Arc::as_ptr(key)))
                .and_then(|(_, value)| value.upgrade())
        };
        if let Some(value) = lookup() {
            return value;
        }
        let value = Arc::new(build());
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((Arc::downgrade(key), Arc::downgrade(&value)));
        value
    }
}

/// Topologies whose structures may be shared, registered by the engines
/// that hold them. A reference to a registered, live topology is the same
/// object as the registered one, so its derived structures can be reused.
static TOPOLOGIES: Mutex<Vec<Weak<TopologyBundle>>> = Mutex::new(Vec::new());

/// Spatial indexes built over registered topologies, with the edges each
/// tree holds.
static SPATIAL_INDEXES: Mutex<Vec<SpatialIndexEntry>> = Mutex::new(Vec::new());

struct SpatialIndexEntry {
    topology: Weak<TopologyBundle>,
    /// Interned `edge_order` of the graph the index was built for.
    edge_order: Weak<[u32]>,
    /// The tree's edges as a bitset.
    indexed: Arc<[u64]>,
    indexed_count: u32,
    index: Weak<EdgeSpatialIndex>,
}

/// Lets graphs built over `topology` share structures with each other.
pub(crate) fn register_topology(topology: &Arc<TopologyBundle>) {
    let mut topologies = TOPOLOGIES.lock().unwrap_or_else(PoisonError::into_inner);
    topologies.retain(|known| known.strong_count() > 0);
    if !topologies
        .iter()
        .any(|known| std::ptr::eq(known.as_ptr(), Arc::as_ptr(topology)))
    {
        topologies.push(Arc::downgrade(topology));
    }
}

fn registered(topology: &TopologyBundle) -> Option<Arc<TopologyBundle>> {
    TOPOLOGIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter_map(Weak::upgrade)
        .find(|known| std::ptr::eq(known.as_ref(), topology))
}

/// The spatial index of the routable edges (those with a finite
/// `edge_costs` entry, which `edge_order` lists) over `topology`. Graphs over
/// one registered topology share an index when they route on the same
/// edges, and a graph whose edges are a subset of an indexed set shares that
/// tree with a mask (the smallest such tree); `build` makes a new tree
/// otherwise.
pub(crate) fn shared_spatial_index(
    topology: &TopologyBundle,
    edge_order: &Arc<[u32]>,
    edge_costs: &[f64],
    build: impl FnOnce() -> EdgeSpatialIndex,
) -> Arc<EdgeSpatialIndex> {
    let Some(owner) = registered(topology) else {
        return Arc::new(build());
    };
    let routable: Arc<[u64]> = routable_edge_set(edge_costs).into();
    let routable_count = routable.iter().map(|word| word.count_ones()).sum::<u32>();
    let shared = {
        let mut entries = SPATIAL_INDEXES
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        entries.retain(|entry| entry.index.strong_count() > 0);
        let candidates = entries.iter().filter(|entry| {
            entry
                .topology
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, &owner))
        });
        let mut best: Option<(&SpatialIndexEntry, Arc<EdgeSpatialIndex>)> = None;
        for entry in candidates {
            let Some(index) = entry.index.upgrade() else {
                continue;
            };
            if entry
                .edge_order
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, edge_order))
            {
                return index;
            }
            let covers = entry.indexed.len() == routable.len()
                && entry
                    .indexed
                    .iter()
                    .zip(routable.iter())
                    .all(|(indexed, needed)| needed & !indexed == 0);
            if covers
                && best
                    .as_ref()
                    .is_none_or(|(known, _)| entry.indexed_count < known.indexed_count)
            {
                best = Some((entry, index));
            }
        }
        best.map(|(_, index)| Arc::new(index.restricted_to(routable.clone())))
    };
    if let Some(index) = shared {
        return index;
    }
    let index = Arc::new(build());
    SPATIAL_INDEXES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(SpatialIndexEntry {
            topology: Arc::downgrade(&owner),
            edge_order: Arc::downgrade(edge_order),
            indexed: routable,
            indexed_count: routable_count,
            index: Arc::downgrade(&index),
        });
    index
}

#[cfg(test)]
mod tests;
