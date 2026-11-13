use serde::{Deserialize, Serialize};

use crate::{CacheBundleId, Column};

/// Algorithm identifier for the current CCH acceleration bundle format.
pub const CCH_ALGORITHM: &str = "edge_based_cch_v3";

/// Schema version of [`DatasetAccelerationBundle`].
pub const ACCELERATION_BUNDLE_SCHEMA_VERSION: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccelerationBundleStats {
    pub base_arc_count: u64,
    pub shortcut_arc_count: u64,
    pub total_arc_count: u64,
}

/// Metric-independent CCH topology built once per dataset, laid out in rank
/// order as hierarchy searches walk it.
///
/// Vertices are directed edge states, numbered by their contraction rank;
/// arcs are legal edge-to-edge transitions plus the complete set of
/// elimination shortcuts. An upward arc's tail ranks below its head, a
/// downward arc's tail above its head. Upward arcs are stored by tail rank,
/// downward arcs by head rank, each row sorted by the other endpoint's rank:
/// forward searches and customization read the rows of the state they
/// visit, backward searches the rows of the state they come from. An arc is
/// identified by its position in [`Self::upward_head`] or
/// [`Self::downward_tail`], and customized weights are aligned with those
/// positions.
///
/// Offsets are 64-bit, so the arc count is not limited by 32-bit
/// positions; state ids (ranks and edge indexes) are 32-bit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetAccelerationBundle {
    pub schema_version: u32,
    pub source_topology_bundle_id: CacheBundleId,
    pub algorithm: String,
    pub stats: AccelerationBundleStats,
    /// Fingerprint of the arc topology ([`Self::compute_hierarchy_key`]).
    /// Customized weights record the key they were computed for, so weights
    /// of another hierarchy are never paired with this one.
    pub hierarchy_key: u64,
    /// Edge state of each rank: the contraction order.
    pub edge_order: Column<u32>,
    /// Rank of each edge state.
    pub edge_rank: Column<u32>,
    /// Upward arcs by tail rank (CSR offsets over [`Self::upward_head`]).
    pub upward_first_out: Column<u64>,
    /// Head rank of each upward arc, ascending within a row.
    pub upward_head: Column<u32>,
    /// Downward arcs by head rank (CSR offsets over [`Self::downward_tail`]).
    pub downward_first_in: Column<u64>,
    /// Tail rank of each downward arc, ascending within a row.
    pub downward_tail: Column<u32>,
    /// The downward arcs again, by tail rank (CSR offsets over
    /// [`Self::downward_head`]): the lower neighbours unpacking tries as
    /// triangle middles.
    pub downward_first_out: Column<u64>,
    /// Head rank of each downward arc in [`Self::downward_first_out`] rows,
    /// ordered by the head's edge index.
    pub downward_head: Column<u32>,
}

impl DatasetAccelerationBundle {
    /// Whether this bundle uses the current CCH algorithm and schema.
    pub fn is_current_format(&self) -> bool {
        self.schema_version == ACCELERATION_BUNDLE_SCHEMA_VERSION && self.algorithm == CCH_ALGORITHM
    }

    /// The hierarchy over `edge_order` (the edge state of each rank) with
    /// the given upward and downward arcs as `(tail, head)` edge indexes, in
    /// any order and without duplicates. Errors when the order is not a
    /// permutation or an arc points the wrong way.
    pub fn from_arcs(
        source_topology_bundle_id: CacheBundleId,
        stats: AccelerationBundleStats,
        edge_order: Vec<u32>,
        upward: &[(u32, u32)],
        downward: &[(u32, u32)],
    ) -> Result<Self, String> {
        let states = edge_order.len();
        check_state_count(states)?;
        let mut edge_rank = vec![u32::MAX; states];
        for (rank, &edge) in edge_order.iter().enumerate() {
            match edge_rank.get_mut(edge as usize) {
                Some(slot) if *slot == u32::MAX => *slot = rank as u32,
                _ => return Err("CCH edge order is not a permutation".to_string()),
            }
        }
        let ranked = |arcs: &[(u32, u32)], upward: bool| -> Result<Vec<(u32, u32)>, String> {
            arcs.iter()
                .map(|&(tail, head)| {
                    let rank = |edge: u32| {
                        edge_rank
                            .get(edge as usize)
                            .copied()
                            .ok_or_else(|| format!("CCH arc references missing edge {edge}"))
                    };
                    let (tail, head) = (rank(tail)?, rank(head)?);
                    if (tail < head) != upward || tail == head {
                        return Err("CCH arc points against its rank direction".to_string());
                    }
                    Ok((tail, head))
                })
                .collect()
        };
        let upward = ranked(upward, true)?;
        let downward = ranked(downward, false)?;
        // Rows of `(row, entry)` pairs, entries in the order `key` sorts.
        let csr = |pairs: &[(u32, u32)], key: &dyn Fn(u32) -> u32| {
            let mut first = vec![0_u64; states + 1];
            for &(row, _) in pairs {
                first[row as usize + 1] += 1;
            }
            for row in 0..states {
                first[row + 1] += first[row];
            }
            let mut cursor = first[..states].to_vec();
            let mut entries = vec![0_u32; pairs.len()];
            for &(row, entry) in pairs {
                let slot = &mut cursor[row as usize];
                entries[*slot as usize] = entry;
                *slot += 1;
            }
            for row in 0..states {
                entries[first[row] as usize..first[row + 1] as usize]
                    .sort_unstable_by_key(|&entry| key(entry));
            }
            (first, entries)
        };
        let by_rank = |rank: u32| rank;
        let by_edge = |rank: u32| edge_order[rank as usize];
        let (upward_first_out, upward_head) = csr(&upward, &by_rank);
        let inverted = downward
            .iter()
            .map(|&(tail, head)| (head, tail))
            .collect::<Vec<_>>();
        let (downward_first_in, downward_tail) = csr(&inverted, &by_rank);
        drop(inverted);
        let (downward_first_out, downward_head) = csr(&downward, &by_edge);
        let mut bundle = Self {
            schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
            source_topology_bundle_id,
            algorithm: CCH_ALGORITHM.to_string(),
            stats,
            hierarchy_key: 0,
            edge_order: edge_order.into(),
            edge_rank: edge_rank.into(),
            upward_first_out: upward_first_out.into(),
            upward_head: upward_head.into(),
            downward_first_in: downward_first_in.into(),
            downward_tail: downward_tail.into(),
            downward_first_out: downward_first_out.into(),
            downward_head: downward_head.into(),
        };
        bundle.hierarchy_key = bundle.compute_hierarchy_key();
        Ok(bundle)
    }

    /// Number of edge states (ranks).
    pub fn state_count(&self) -> usize {
        self.edge_order.len()
    }

    /// Positions of the upward arcs leaving `rank`.
    #[inline]
    pub fn upward_range(&self, rank: usize) -> std::ops::Range<usize> {
        self.upward_first_out[rank] as usize..self.upward_first_out[rank + 1] as usize
    }

    /// Positions of the downward arcs entering `rank`.
    #[inline]
    pub fn downward_range(&self, rank: usize) -> std::ops::Range<usize> {
        self.downward_first_in[rank] as usize..self.downward_first_in[rank + 1] as usize
    }

    /// Indexes into [`Self::downward_head`] of the downward arcs leaving
    /// `rank`.
    #[inline]
    pub fn downward_out_range(&self, rank: usize) -> std::ops::Range<usize> {
        self.downward_first_out[rank] as usize..self.downward_first_out[rank + 1] as usize
    }

    /// Position of the upward arc `tail -> head` (ranks), if any.
    #[inline]
    pub fn upward_position(&self, tail: u32, head: u32) -> Option<usize> {
        let range = self.upward_range(tail as usize);
        self.upward_head[range.clone()]
            .binary_search(&head)
            .ok()
            .map(|offset| range.start + offset)
    }

    /// Position of the downward arc `tail -> head` (ranks), if any.
    #[inline]
    pub fn downward_position(&self, tail: u32, head: u32) -> Option<usize> {
        let range = self.downward_range(head as usize);
        self.downward_tail[range.clone()]
            .binary_search(&tail)
            .ok()
            .map(|offset| range.start + offset)
    }

    /// Checks that every offset array fits the state count and addresses
    /// all of its entries, and that the rank table has one entry per state.
    pub fn validate(&self) -> Result<(), String> {
        let states = self.state_count();
        check_state_count(states)?;
        if self.edge_rank.len() != states {
            return Err("CCH rank table does not match the order".to_string());
        }
        for (name, first, entries) in [
            ("upward", &self.upward_first_out, self.upward_head.len()),
            (
                "downward",
                &self.downward_first_in,
                self.downward_tail.len(),
            ),
            (
                "downward out",
                &self.downward_first_out,
                self.downward_head.len(),
            ),
        ] {
            if first.len() != states + 1
                || first.first().is_some_and(|start| *start != 0)
                || first.last().map(|end| *end as usize) != Some(entries)
            {
                return Err(format!("CCH {name} offsets do not address their arcs"));
            }
        }
        if self.downward_tail.len() != self.downward_head.len() {
            return Err("CCH downward arc indexes differ in length".to_string());
        }
        Ok(())
    }

    /// A 64-bit fingerprint of the order and every arc array.
    pub fn compute_hierarchy_key(&self) -> u64 {
        let mut hash = FingerprintHasher::default();
        for column in [
            &self.edge_order,
            &self.edge_rank,
            &self.upward_head,
            &self.downward_tail,
            &self.downward_head,
        ] {
            hash.write_u64(column.len() as u64);
            for &value in column.iter() {
                hash.write_u32(value);
            }
        }
        for column in [
            &self.upward_first_out,
            &self.downward_first_in,
            &self.downward_first_out,
        ] {
            hash.write_u64(column.len() as u64);
            for &value in column.iter() {
                hash.write_u64(value);
            }
        }
        hash.finish()
    }
}

/// Ranks are 32-bit and searches reserve `u32::MAX` as "no state", so a
/// hierarchy holds fewer than `u32::MAX` states. Arc counts are unbounded.
fn check_state_count(states: usize) -> Result<(), String> {
    if states >= u32::MAX as usize {
        return Err(format!(
            "{states} edge states exceed the {} a CCH hierarchy addresses with 32-bit ranks",
            u32::MAX - 1
        ));
    }
    Ok(())
}

/// FNV-1a style mixing over 32-bit words, finished with a 64-bit avalanche.
struct FingerprintHasher(u64);

impl Default for FingerprintHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl FingerprintHasher {
    fn write_u32(&mut self, value: u32) {
        self.0 = (self.0 ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn write_u64(&mut self, value: u64) {
        self.write_u32(value as u32);
        self.write_u32((value >> 32) as u32);
    }

    fn finish(&self) -> u64 {
        let mut value = self.0;
        value ^= value >> 33;
        value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
        value ^= value >> 33;
        value = value.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        value ^ (value >> 33)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchies_keep_u32_max_free_as_a_rank_sentinel() {
        assert!(check_state_count(u32::MAX as usize - 1).is_ok());
        assert!(check_state_count(u32::MAX as usize).is_err());
        assert!(check_state_count(u32::MAX as usize + 1).is_err());
    }

    #[test]
    fn from_arcs_lays_out_rank_ordered_rows() {
        // Order 2, 0, 1: edge 2 has rank 0, edge 0 rank 1, edge 1 rank 2.
        let bundle = DatasetAccelerationBundle::from_arcs(
            CacheBundleId::new("t"),
            AccelerationBundleStats::default(),
            vec![2, 0, 1],
            &[(2, 1), (2, 0), (0, 1)],
            &[(1, 2), (1, 0), (0, 2)],
        )
        .expect("valid hierarchy");
        bundle.validate().expect("consistent");
        assert_eq!(bundle.edge_rank, vec![1, 2, 0]);
        // Upward rows by tail rank, heads ascending by rank.
        assert_eq!(bundle.upward_first_out, vec![0, 2, 3, 3]);
        assert_eq!(bundle.upward_head, vec![1, 2, 2]);
        // Downward arcs by head rank, tails ascending by rank.
        assert_eq!(bundle.downward_first_in, vec![0, 2, 3, 3]);
        assert_eq!(bundle.downward_tail, vec![1, 2, 2]);
        // By tail rank, heads in edge order: edge 1 (rank 2) reaches
        // edge 0 (rank 1) before edge 2 (rank 0).
        assert_eq!(bundle.downward_first_out, vec![0, 0, 1, 3]);
        assert_eq!(bundle.downward_head, vec![0, 1, 0]);
        assert_eq!(bundle.upward_position(0, 2), Some(1));
        assert_eq!(bundle.downward_position(2, 1), Some(2));
        assert_eq!(bundle.downward_position(1, 2), None);
        assert_eq!(bundle.hierarchy_key, bundle.compute_hierarchy_key());

        let reversed = DatasetAccelerationBundle::from_arcs(
            CacheBundleId::new("t"),
            AccelerationBundleStats::default(),
            vec![2, 0, 1],
            &[(1, 2)],
            &[],
        );
        assert!(reversed.is_err());
    }
}
