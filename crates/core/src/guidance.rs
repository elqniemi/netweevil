//! Street names, route numbers, signposts and lane tags kept for
//! turn-by-turn guidance. They live in the lazily loaded edge-name bundle,
//! not in the topology, because only directions and segment rows read them.

use serde::{Deserialize, Serialize};

/// Schema version of [`EdgeNameBundle`]. Readers accept this value only.
pub const EDGE_NAME_BUNDLE_SCHEMA_VERSION: u32 = 2;

/// Guidance tags of one directed edge. Every field indexes
/// [`EdgeNameBundle::strings`]; values keep their OSM `;` separators.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeGuidance {
    /// `ref`: the route number, such as `A7` or `N355`.
    pub reference: Option<u32>,
    /// `turn:lanes` for this travel direction, listed left to right.
    pub turn_lanes: Option<u32>,
    /// `destination` for this travel direction.
    pub destination: Option<u32>,
    /// `destination:ref` for this travel direction.
    pub destination_ref: Option<u32>,
    /// `destination:street` for this travel direction.
    pub destination_street: Option<u32>,
    /// Exit number: `junction:ref` on the way, or the `ref` of the
    /// `highway=motorway_junction` node a link edge leaves.
    pub junction_ref: Option<u32>,
    /// `name` of the motorway junction node the edge leaves.
    pub junction_name: Option<u32>,
    /// `exit_to` on the way or on the junction node a link edge leaves.
    pub exit_to: Option<u32>,
}

impl EdgeGuidance {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Edge names plus guidance records, shared by every profile of a dataset.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EdgeNameBundle {
    /// Street names indexed by `EdgePresentation::name_index`.
    pub names: Vec<String>,
    /// Interned guidance strings referenced by [`EdgeGuidance`] fields.
    pub strings: Vec<String>,
    /// Distinct guidance records. Index 0 is the empty record.
    pub guidance_records: Vec<EdgeGuidance>,
    /// Record index per edge, `0` for edges without guidance tags. Empty
    /// when the source carries no guidance tags at all.
    pub edge_guidance: Vec<u32>,
}

impl EdgeNameBundle {
    /// Bundle with names only, for sources without guidance tags.
    pub fn from_names(names: Vec<String>) -> Self {
        Self {
            names,
            ..Self::default()
        }
    }

    pub fn guidance(&self, edge_index: usize) -> EdgeGuidance {
        self.edge_guidance
            .get(edge_index)
            .and_then(|&record| self.guidance_records.get(record as usize))
            .copied()
            .unwrap_or_default()
    }

    pub fn string(&self, index: Option<u32>) -> Option<&str> {
        index.and_then(|index| self.strings.get(index as usize).map(String::as_str))
    }

    /// Checks that every index points into its table.
    pub fn validate(&self, edge_count: Option<usize>) -> Result<(), String> {
        if let Some(edge_count) = edge_count
            && !self.edge_guidance.is_empty()
            && self.edge_guidance.len() != edge_count
        {
            return Err(format!(
                "edge guidance covers {} edges but the topology has {edge_count}",
                self.edge_guidance.len()
            ));
        }
        if let Some(&record) = self
            .edge_guidance
            .iter()
            .find(|&&record| record as usize >= self.guidance_records.len().max(1))
        {
            return Err(format!("edge guidance record {record} is out of range"));
        }
        let strings = self.strings.len();
        for record in &self.guidance_records {
            let fields = [
                record.reference,
                record.turn_lanes,
                record.destination,
                record.destination_ref,
                record.destination_street,
                record.junction_ref,
                record.junction_name,
                record.exit_to,
            ];
            if fields
                .into_iter()
                .flatten()
                .any(|index| index as usize >= strings)
            {
                return Err("guidance string index is out of range".into());
            }
        }
        Ok(())
    }
}
