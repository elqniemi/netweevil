//! Editable OSM source state: every node position, the routable ways, the
//! turn-restriction relations and the traffic-signal nodes of an extract.
//!
//! A PBF import scans this state, derives its [`ScanOutput`] for the
//! topology build and persists it as the dataset's source snapshot. An
//! update restores it from the snapshot, applies change files and derives
//! the next scan output from the same code, so an incremental update builds
//! the topology a full import of the changed extract would build.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use netweevil_core::{CacheBundleId, TemporalRuleSet, TopologyBundle};
use netweevil_persist::{SOURCE_SNAPSHOT_SCHEMA_VERSION, SourceSnapshot};
use osmpbfreader::{OsmObj, OsmPbfReader};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::access::ConditionalCounts;
use crate::admin::{
    AdminBoundary, AdminRelationCandidate, assemble_boundaries, boundary_node_ids,
    parse_admin_relation,
};
use crate::classify::is_traffic_signal_node;
use crate::guidance::{JunctionNodeTags, junction_node_tags};
use crate::import::{
    CountingReader, DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress,
};
use crate::osm_change::{ChangeAction, OsmChange};
use crate::restrictions::{TurnRestrictionCandidate, ViaSpec, parse_turn_restriction_relation};
use crate::scan::{
    ObjectCounts, PendingWay, RuleSetInterner, ScanOutput, is_structure_way, pending_osm_way,
};
use crate::topology::EnrichmentInputs;

/// The snapshot record types the ingest persists.
pub(crate) type OsmSourceSnapshot =
    SourceSnapshot<PendingWay, TurnRestrictionCandidate, SnapshotExtras>;

/// Classified source data the snapshot keeps beside ways and restrictions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SnapshotExtras {
    /// Guidance tags of motorway-junction nodes, sorted by node id.
    junction_nodes: Vec<(i64, JunctionNodeTags)>,
    /// Temporal rule sets that way and restriction records refer to, in the
    /// order a full import interns them.
    rule_sets: Vec<TemporalRuleSet>,
    /// Routable ways on bridges or in tunnels, sorted.
    structure_way_ids: Vec<i64>,
    /// Admin boundary relations, by id.
    admin_relations: Vec<AdminRelationCandidate>,
    /// The boundaries file and DEM the dataset was built with, by path and
    /// SHA-256; updates apply them again.
    enrichment: EnrichmentInputs,
}

impl SnapshotExtras {
    /// The enrichment inputs of the build the snapshot belongs to.
    pub(crate) fn enrichment(&self) -> &EnrichmentInputs {
        &self.enrichment
    }
}

/// One source node in 1e-7 degree units, as PBF files store positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceNode {
    id: i64,
    lon_e7: i32,
    lat_e7: i32,
}

impl SourceNode {
    /// Degrees exactly as `osmpbfreader::Node::lon`/`lat` compute them.
    fn lon_lat(self) -> (f64, f64) {
        (self.lon_e7 as f64 * 1e-7, self.lat_e7 as f64 * 1e-7)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OsmSourceState {
    /// Every source node, sorted by id with unique ids.
    nodes: Vec<SourceNode>,
    traffic_signals: FxHashSet<i64>,
    /// Guidance tags of every motorway-junction node.
    junction_nodes: FxHashMap<i64, JunctionNodeTags>,
    /// Routable ways by id.
    ways: BTreeMap<i64, PendingWay>,
    /// Turn-restriction relations by id.
    /// Turn-restriction relations by id, one candidate per distinct kind,
    /// mode set and condition.
    restrictions: BTreeMap<i64, Vec<TurnRestrictionCandidate>>,
    /// Conditional access, speed and restriction rule sets, numbered as a
    /// full import of the current objects numbers them.
    rule_sets: RuleSetInterner,
    /// Ids of the ways that are not routable.
    other_ways: BTreeMap<i64, Vec<i64>>,
    /// Routable ways on bridges or in tunnels.
    structure_ways: FxHashSet<i64>,
    /// Admin boundary relations (`boundary=administrative`), a subset of
    /// the other relations.
    admin_relations: BTreeMap<i64, AdminRelationCandidate>,
    /// Ids of the relations that are not turn restrictions.
    other_relations: FxHashSet<i64>,
}

/// Objects one change set touched, after keeping each object's newest
/// version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChangeSummary {
    pub(crate) nodes: u64,
    pub(crate) ways: u64,
    pub(crate) relations: u64,
}

impl OsmSourceState {
    /// Scans an OSM PBF extract in one pass.
    pub(crate) fn scan_pbf(
        source_path: &Path,
        source_size_bytes: u64,
        progress: &mut impl FnMut(DatasetImportProgress),
    ) -> Result<Self> {
        let file = File::open(source_path)
            .with_context(|| format!("opening dataset source {}", source_path.display()))?;
        let bytes_read = Arc::new(AtomicU64::new(0));
        let mut reader = OsmPbfReader::new(CountingReader::new(file, Arc::clone(&bytes_read)));
        let mut state = Self::default();
        let mut conditionals = ConditionalCounts::default();
        let mut reporter = PercentReporter::starting_at_zero();
        emit_progress(
            progress,
            DatasetImportStage::ScanRoutableObjects,
            Some(0.0),
            "Scanning OSM objects 0%".to_string(),
        );
        for object in reader.iter() {
            state.insert_scanned(object.context("reading PBF object")?, &mut conditionals);
            reporter.emit_if_needed(
                bytes_read.load(Ordering::Relaxed),
                source_size_bytes,
                DatasetImportStage::ScanRoutableObjects,
                progress,
                |percent| {
                    format!(
                        "Scanning OSM objects {percent:.0}% ({} nodes, {} routable ways, {} restrictions)",
                        state.nodes.len(),
                        state.ways.len(),
                        state.restrictions.len()
                    )
                },
            );
        }
        state.normalize_nodes();
        state.renumber_rule_sets();
        emit_progress(
            progress,
            DatasetImportStage::ScanRoutableObjects,
            Some(100.0),
            format!(
                "Scanning OSM objects 100% ({} nodes, {} routable ways, {} restrictions, {} \
                 conditional tags imported, {} skipped as unsupported)",
                state.nodes.len(),
                state.ways.len(),
                state.restrictions.len(),
                conditionals.imported,
                conditionals.skipped
            ),
        );
        Ok(state)
    }

    /// Builds the state from objects in source order, as a PBF scan would.
    #[cfg(test)]
    pub(crate) fn from_objects(objects: impl IntoIterator<Item = OsmObj>) -> Self {
        let mut state = Self::default();
        let mut conditionals = ConditionalCounts::default();
        for object in objects {
            state.insert_scanned(object, &mut conditionals);
        }
        state.normalize_nodes();
        state.renumber_rule_sets();
        state
    }

    fn insert_scanned(&mut self, object: OsmObj, conditionals: &mut ConditionalCounts) {
        match object {
            OsmObj::Node(node) => {
                self.set_node_tags(node.id.0, Some(&node.tags));
                self.nodes.push(SourceNode {
                    id: node.id.0,
                    lon_e7: node.decimicro_lon,
                    lat_e7: node.decimicro_lat,
                });
            }
            OsmObj::Way(way) => self.upsert_way(way, conditionals),
            OsmObj::Relation(relation) => self.upsert_relation(&relation),
        }
    }

    /// Records what node tags classify to; `None` removes the node's entries.
    fn set_node_tags(&mut self, id: i64, tags: Option<&osmpbfreader::Tags>) {
        if tags.is_some_and(is_traffic_signal_node) {
            self.traffic_signals.insert(id);
        } else {
            self.traffic_signals.remove(&id);
        }
        match tags.and_then(junction_node_tags) {
            Some(junction) => {
                self.junction_nodes.insert(id, junction);
            }
            None => {
                self.junction_nodes.remove(&id);
            }
        }
    }

    fn upsert_way(&mut self, way: osmpbfreader::Way, conditionals: &mut ConditionalCounts) {
        let id = way.id.0;
        let node_ids = way.nodes.iter().map(|node| node.0).collect::<Vec<_>>();
        let pending = pending_osm_way(
            id,
            node_ids.clone(),
            &way.tags,
            &mut self.rule_sets,
            conditionals,
        );
        match pending {
            Some(pending) => {
                self.other_ways.remove(&id);
                if is_structure_way(&way.tags) {
                    self.structure_ways.insert(id);
                } else {
                    self.structure_ways.remove(&id);
                }
                self.ways.insert(id, pending);
            }
            None => {
                self.ways.remove(&id);
                self.structure_ways.remove(&id);
                // Kept for admin boundaries assembled from member ways.
                self.other_ways.insert(id, node_ids);
            }
        }
    }

    fn delete_way(&mut self, id: i64) {
        self.ways.remove(&id);
        self.other_ways.remove(&id);
        self.structure_ways.remove(&id);
    }

    fn upsert_relation(&mut self, relation: &osmpbfreader::Relation) {
        let id = relation.id.0;
        let candidates = parse_turn_restriction_relation(relation, &mut self.rule_sets);
        if candidates.is_empty() {
            self.restrictions.remove(&id);
            self.other_relations.insert(id);
            match parse_admin_relation(relation) {
                Some(admin) => {
                    self.admin_relations.insert(id, admin);
                }
                None => {
                    self.admin_relations.remove(&id);
                }
            }
        } else {
            self.other_relations.remove(&id);
            self.admin_relations.remove(&id);
            self.restrictions.insert(id, candidates);
        }
    }

    fn delete_relation(&mut self, id: i64) {
        self.restrictions.remove(&id);
        self.other_relations.remove(&id);
        self.admin_relations.remove(&id);
    }

    /// Renumbers the rule sets in the order a full import interns them:
    /// ways by id (forward before reverse), then restriction relations by
    /// id. Drops sets nothing refers to any more.
    fn renumber_rule_sets(&mut self) {
        let sets = std::mem::take(&mut self.rule_sets).into_sets();
        let mut renumbered = RuleSetInterner::default();
        let mut renumber = |id: u32| renumbered.intern(sets[id as usize].clone());
        for way in self.ways.values_mut() {
            way.forward_temporal_rule_id = way.forward_temporal_rule_id.map(&mut renumber);
            way.reverse_temporal_rule_id = way.reverse_temporal_rule_id.map(&mut renumber);
        }
        for candidate in self.restrictions.values_mut().flatten() {
            if let Some(condition) = candidate.condition.as_mut() {
                condition.rule_set_id = renumber(condition.rule_set_id);
            }
        }
        self.rule_sets = renumbered;
    }

    /// Number of source nodes, ways and relations, routable or not.
    fn counts(&self) -> ObjectCounts {
        ObjectCounts {
            nodes: self.nodes.len() as u64,
            ways: (self.ways.len() + self.other_ways.len()) as u64,
            relations: (self.restrictions.len() + self.other_relations.len()) as u64,
        }
    }

    /// Sorts nodes by id when the source was not, keeping the last
    /// occurrence of a repeated id.
    fn normalize_nodes(&mut self) {
        if self.nodes.windows(2).all(|pair| pair[0].id < pair[1].id) {
            return;
        }
        self.nodes.sort_by_key(|node| node.id);
        let mut kept: Vec<SourceNode> = Vec::with_capacity(self.nodes.len());
        for node in self.nodes.drain(..) {
            match kept.last_mut() {
                Some(last) if last.id == node.id => *last = node,
                _ => kept.push(node),
            }
        }
        self.nodes = kept;
    }

    fn node(&self, id: i64) -> Option<SourceNode> {
        self.nodes
            .binary_search_by_key(&id, |node| node.id)
            .ok()
            .map(|index| self.nodes[index])
    }

    /// Applies one change file's effective changes: deletions remove the
    /// object, creations and modifications replace it with the new version.
    pub(crate) fn apply_changes(&mut self, changes: Vec<OsmChange>) -> ChangeSummary {
        let mut summary = ChangeSummary::default();
        let mut node_updates = BTreeMap::<i64, Option<SourceNode>>::new();
        for change in changes {
            let deleted = change.action == ChangeAction::Delete;
            match change.object {
                OsmObj::Node(node) => {
                    summary.nodes += 1;
                    let id = node.id.0;
                    self.set_node_tags(id, (!deleted).then_some(&node.tags));
                    node_updates.insert(
                        id,
                        (!deleted).then_some(SourceNode {
                            id,
                            lon_e7: node.decimicro_lon,
                            lat_e7: node.decimicro_lat,
                        }),
                    );
                }
                OsmObj::Way(way) => {
                    summary.ways += 1;
                    if deleted {
                        self.delete_way(way.id.0);
                    } else {
                        self.upsert_way(way, &mut ConditionalCounts::default());
                    }
                }
                OsmObj::Relation(relation) => {
                    summary.relations += 1;
                    if deleted {
                        self.delete_relation(relation.id.0);
                    } else {
                        self.upsert_relation(&relation);
                    }
                }
            }
        }
        self.merge_node_updates(node_updates);
        self.renumber_rule_sets();
        summary
    }

    fn merge_node_updates(&mut self, updates: BTreeMap<i64, Option<SourceNode>>) {
        if updates.is_empty() {
            return;
        }
        let mut merged = Vec::with_capacity(self.nodes.len() + updates.len());
        let mut updates = updates.into_iter().peekable();
        for node in self.nodes.drain(..) {
            while let Some((_, update)) = updates.next_if(|(id, _)| *id < node.id) {
                merged.extend(update);
            }
            match updates.next_if(|(id, _)| *id == node.id) {
                Some((_, update)) => merged.extend(update),
                None => merged.push(node),
            }
        }
        merged.extend(updates.filter_map(|(_, update)| update));
        self.nodes = merged;
    }

    /// The scan output a PBF scan of this state's objects produces, plus the
    /// OSM ids of the topology nodes in topology node order. With
    /// `collect_admin`, admin boundaries are assembled from the retained
    /// admin relations and their member ways.
    pub(crate) fn scan_output(&self, collect_admin: bool) -> (ScanOutput, Vec<i64>) {
        let mut needed = Vec::new();
        for way in self.ways.values() {
            needed.extend_from_slice(&way.node_ids);
        }
        for candidate in self.restrictions.values().flatten() {
            if let ViaSpec::Node(node_id) = candidate.via {
                needed.push(node_id);
            }
        }
        needed.sort_unstable();
        needed.dedup();
        let (admin_boundaries, incomplete_admin_relations) = if collect_admin {
            self.admin_boundaries()
        } else {
            (Vec::new(), Vec::new())
        };
        let junction_nodes = needed
            .iter()
            .filter_map(|id| Some((*id, self.junction_nodes.get(id)?.clone())))
            .collect();
        let mut node_coords = FxHashMap::with_capacity_and_hasher(needed.len(), Default::default());
        let mut topology_node_ids = Vec::with_capacity(needed.len());
        for id in needed {
            if let Some(node) = self.node(id) {
                let (lon, lat) = node.lon_lat();
                node_coords.insert(id, (lon, lat, 0.0));
                topology_node_ids.push(id);
            }
        }
        let scan = ScanOutput {
            pending_ways: self.ways.values().cloned().collect(),
            restriction_candidates: self.restrictions.values().flatten().cloned().collect(),
            node_coords,
            traffic_signal_nodes: self.traffic_signals.clone(),
            junction_nodes,
            counts: self.counts(),
            feature_attributes: Default::default(),
            temporal_rule_sets: self.rule_sets.sets().to_vec(),
            structure_ways: self.structure_ways.clone(),
            admin_boundaries,
            incomplete_admin_relations,
        };
        (scan, topology_node_ids)
    }

    /// Admin boundaries assembled from the admin relations, and the ids of
    /// relations that could not be assembled.
    fn admin_boundaries(&self) -> (Vec<AdminBoundary>, Vec<i64>) {
        let candidates = self.admin_relations.values().cloned().collect::<Vec<_>>();
        let mut way_nodes = FxHashMap::<i64, Vec<i64>>::default();
        for way_id in candidates
            .iter()
            .flat_map(|candidate| candidate.member_way_ids.iter())
        {
            let nodes = self
                .ways
                .get(way_id)
                .map(|way| &way.node_ids)
                .or_else(|| self.other_ways.get(way_id));
            if let Some(nodes) = nodes {
                way_nodes.insert(*way_id, nodes.clone());
            }
        }
        let boundary_coords = boundary_node_ids(&way_nodes)
            .into_iter()
            .filter_map(|id| Some((id, self.node(id)?.lon_lat())))
            .collect::<FxHashMap<_, _>>();
        assemble_boundaries(&candidates, &way_nodes, &boundary_coords)
    }

    /// The persisted form of this state. `topology_node_ids` are the ids
    /// [`Self::scan_output`] returned for the topology built from it, and
    /// `enrichment` the inputs that build used.
    pub(crate) fn to_snapshot(
        &self,
        source_topology_bundle_id: CacheBundleId,
        topology_node_ids: &[i64],
        enrichment: &EnrichmentInputs,
    ) -> OsmSourceSnapshot {
        let mut other_node_ids = Vec::with_capacity(self.nodes.len() - topology_node_ids.len());
        let mut other_node_lon_e7 = Vec::with_capacity(other_node_ids.capacity());
        let mut other_node_lat_e7 = Vec::with_capacity(other_node_ids.capacity());
        let mut topology_ids = topology_node_ids.iter().peekable();
        for node in &self.nodes {
            if topology_ids.next_if_eq(&&node.id).is_none() {
                other_node_ids.push(node.id);
                other_node_lon_e7.push(node.lon_e7);
                other_node_lat_e7.push(node.lat_e7);
            }
        }
        let traffic_signal_node_ids = sorted(&self.traffic_signals);

        let mut names = Vec::new();
        let mut name_lookup = HashMap::<&str, u32>::new();
        let mut way_ids = Vec::with_capacity(self.ways.len());
        let mut way_node_counts = Vec::with_capacity(self.ways.len());
        let mut way_node_ids = Vec::new();
        let mut way_name_indexes = Vec::with_capacity(self.ways.len());
        let mut way_attributes = Vec::with_capacity(self.ways.len());
        for (&id, way) in &self.ways {
            way_ids.push(id);
            way_node_counts.push(way.node_ids.len() as u32);
            way_node_ids.extend_from_slice(&way.node_ids);
            way_name_indexes.push(way.name.as_deref().map(|name| {
                *name_lookup.entry(name).or_insert_with(|| {
                    names.push(name.to_string());
                    names.len() as u32 - 1
                })
            }));
            way_attributes.push(PendingWay {
                osm_way_id: 0,
                node_ids: Vec::new(),
                name: None,
                ..way.clone()
            });
        }

        SourceSnapshot {
            schema_version: SOURCE_SNAPSHOT_SCHEMA_VERSION,
            source_topology_bundle_id,
            topology_node_ids: topology_node_ids.to_vec(),
            other_node_ids,
            other_node_lon_e7,
            other_node_lat_e7,
            traffic_signal_node_ids,
            way_ids,
            way_node_counts,
            way_node_ids,
            way_name_indexes,
            names,
            way_attributes,
            restrictions: self.restrictions.values().flatten().cloned().collect(),
            other_way_ids: self.other_ways.keys().copied().collect(),
            other_way_node_counts: self
                .other_ways
                .values()
                .map(|nodes| nodes.len() as u32)
                .collect(),
            other_way_node_ids: self.other_ways.values().flatten().copied().collect(),
            other_relation_ids: sorted(&self.other_relations),
            extras: SnapshotExtras {
                junction_nodes: {
                    let mut junctions = self
                        .junction_nodes
                        .iter()
                        .map(|(id, tags)| (*id, tags.clone()))
                        .collect::<Vec<_>>();
                    junctions.sort_unstable_by_key(|(id, _)| *id);
                    junctions
                },
                rule_sets: self.rule_sets.sets().to_vec(),
                structure_way_ids: sorted(&self.structure_ways),
                admin_relations: self.admin_relations.values().cloned().collect(),
                enrichment: enrichment.clone(),
            },
        }
    }

    /// Restores the state from a snapshot and the topology built with it,
    /// which holds the topology nodes' coordinates.
    pub(crate) fn from_snapshot(
        snapshot: OsmSourceSnapshot,
        topology: &TopologyBundle,
    ) -> Result<Self> {
        if snapshot.topology_node_ids.len() != topology.nodes.len() {
            bail!(
                "source snapshot lists {} topology nodes, the topology has {}",
                snapshot.topology_node_ids.len(),
                topology.nodes.len()
            );
        }
        let topology_nodes = snapshot
            .topology_node_ids
            .iter()
            .zip(&topology.nodes)
            .map(|(&id, node)| {
                Ok(SourceNode {
                    id,
                    lon_e7: decimicro(node.lon)
                        .context("topology longitude is not an OSM position")?,
                    lat_e7: decimicro(node.lat)
                        .context("topology latitude is not an OSM position")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let other_nodes = snapshot
            .other_node_ids
            .iter()
            .zip(&snapshot.other_node_lon_e7)
            .zip(&snapshot.other_node_lat_e7)
            .map(|((&id, &lon_e7), &lat_e7)| SourceNode { id, lon_e7, lat_e7 });
        let mut nodes = Vec::with_capacity(topology_nodes.len() + snapshot.other_node_ids.len());
        let mut topology_nodes = topology_nodes.into_iter().peekable();
        for node in other_nodes {
            while let Some(topology_node) =
                topology_nodes.next_if(|candidate| candidate.id < node.id)
            {
                nodes.push(topology_node);
            }
            nodes.push(node);
        }
        nodes.extend(topology_nodes);
        if !nodes.windows(2).all(|pair| pair[0].id < pair[1].id) {
            bail!("source snapshot node ids are not unique and sorted");
        }

        let mut ways = BTreeMap::new();
        let mut node_refs = snapshot.way_node_ids.into_iter();
        for (((id, count), name_index), attributes) in snapshot
            .way_ids
            .into_iter()
            .zip(snapshot.way_node_counts)
            .zip(snapshot.way_name_indexes)
            .zip(snapshot.way_attributes)
        {
            ways.insert(
                id,
                PendingWay {
                    osm_way_id: id,
                    node_ids: node_refs.by_ref().take(count as usize).collect(),
                    name: name_index.map(|index| snapshot.names[index as usize].clone()),
                    ..attributes
                },
            );
        }
        let mut restrictions = BTreeMap::<i64, Vec<TurnRestrictionCandidate>>::new();
        for candidate in snapshot.restrictions {
            restrictions
                .entry(candidate.relation_id)
                .or_default()
                .push(candidate);
        }
        let mut other_way_refs = snapshot.other_way_node_ids.into_iter();
        let other_ways = snapshot
            .other_way_ids
            .into_iter()
            .zip(snapshot.other_way_node_counts)
            .map(|(id, count)| (id, other_way_refs.by_ref().take(count as usize).collect()))
            .collect();
        let mut rule_sets = RuleSetInterner::default();
        for (index, set) in snapshot.extras.rule_sets.iter().enumerate() {
            if rule_sets.intern(set.clone()) as usize != index {
                bail!("source snapshot repeats a temporal rule set");
            }
        }
        Ok(Self {
            nodes,
            traffic_signals: snapshot.traffic_signal_node_ids.into_iter().collect(),
            junction_nodes: snapshot.extras.junction_nodes.into_iter().collect(),
            ways,
            restrictions,
            rule_sets,
            other_ways,
            other_relations: snapshot.other_relation_ids.into_iter().collect(),
            structure_ways: snapshot.extras.structure_way_ids.into_iter().collect(),
            admin_relations: snapshot
                .extras
                .admin_relations
                .into_iter()
                .map(|candidate| (candidate.relation_id, candidate))
                .collect(),
        })
    }
}

fn sorted(ids: &FxHashSet<i64>) -> Vec<i64> {
    let mut ids = ids.iter().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

/// The 1e-7 degree value that decodes to exactly `degrees`.
fn decimicro(degrees: f64) -> Option<i32> {
    let units = (degrees * 1e7).round();
    let units = i32::try_from(units as i64).ok()?;
    ((units as f64 * 1e-7).to_bits() == degrees.to_bits()).then_some(units)
}

#[cfg(test)]
mod tests;
