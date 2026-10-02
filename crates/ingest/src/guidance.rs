//! Capture of the OSM tags that turn-by-turn guidance needs: route numbers,
//! turn lanes, destination signposts and motorway exit numbers. Values are
//! kept verbatim (with their `;` and `|` separators) and interned once per
//! dataset, so the edge-name bundle stores each distinct string and each
//! distinct per-edge combination only once.

use std::collections::HashMap;

use netweevil_core::{EdgeGuidance, EdgeNameBundle, HighwayClass};
use osmpbfreader::Tags;

use crate::classify::tag;

/// Guidance tags of one travel direction of a way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirectionalGuidanceTags {
    pub(crate) turn_lanes: Option<String>,
    pub(crate) destination: Option<String>,
    pub(crate) destination_ref: Option<String>,
    pub(crate) destination_street: Option<String>,
}

/// Guidance tags of one way. `None` on [`crate::scan::PendingWay`] when the
/// way carries none of them, which is the common case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WayGuidanceTags {
    pub(crate) reference: Option<String>,
    pub(crate) junction_ref: Option<String>,
    pub(crate) exit_to: Option<String>,
    pub(crate) forward: DirectionalGuidanceTags,
    pub(crate) backward: DirectionalGuidanceTags,
}

/// Tags of a `highway=motorway_junction` node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct JunctionNodeTags {
    pub(crate) reference: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) exit_to: Option<String>,
}

fn value(tags: &Tags, key: &str) -> Option<String> {
    tag(tags, key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Reads the guidance tags of a way. `forward_oneway` / `backward_oneway`
/// say whether vehicles may only travel in that direction: unsuffixed
/// `turn:lanes` and `destination*` tags describe the single travel
/// direction of a one-way road and are ignored on two-way roads, where OSM
/// uses the `:forward` / `:backward` forms.
pub(crate) fn way_guidance_tags(
    tags: &Tags,
    forward_oneway: bool,
    backward_oneway: bool,
) -> Option<Box<WayGuidanceTags>> {
    let directional = |suffix: &str, unsuffixed: bool| {
        let read = |key: &str| {
            value(tags, &format!("{key}:{suffix}"))
                .or_else(|| unsuffixed.then(|| value(tags, key)).flatten())
        };
        DirectionalGuidanceTags {
            turn_lanes: read("turn:lanes"),
            destination: read("destination"),
            destination_ref: read("destination:ref"),
            destination_street: read("destination:street"),
        }
    };
    let guidance = WayGuidanceTags {
        reference: value(tags, "ref"),
        junction_ref: value(tags, "junction:ref"),
        exit_to: value(tags, "exit_to"),
        forward: directional("forward", forward_oneway),
        backward: directional("backward", backward_oneway),
    };
    (guidance != WayGuidanceTags::default()).then(|| Box::new(guidance))
}

pub(crate) fn junction_node_tags(tags: &Tags) -> Option<JunctionNodeTags> {
    if tag(tags, "highway") != Some("motorway_junction") {
        return None;
    }
    let junction = JunctionNodeTags {
        reference: value(tags, "ref"),
        name: value(tags, "name"),
        exit_to: value(tags, "exit_to"),
    };
    (junction != JunctionNodeTags::default()).then_some(junction)
}

const fn is_link(highway: HighwayClass) -> bool {
    matches!(
        highway,
        HighwayClass::MotorwayLink
            | HighwayClass::TrunkLink
            | HighwayClass::PrimaryLink
            | HighwayClass::SecondaryLink
            | HighwayClass::TertiaryLink
    )
}

/// Builds the per-edge guidance columns of the edge-name bundle.
#[derive(Default)]
pub(crate) struct GuidanceInterner {
    strings: Vec<String>,
    string_lookup: HashMap<String, u32>,
    records: Vec<EdgeGuidance>,
    record_lookup: HashMap<EdgeGuidance, u32>,
    edge_records: Vec<u32>,
}

impl GuidanceInterner {
    pub(crate) fn new() -> Self {
        Self {
            records: vec![EdgeGuidance::default()],
            record_lookup: HashMap::from([(EdgeGuidance::default(), 0)]),
            ..Self::default()
        }
    }

    fn string(&mut self, value: Option<&String>) -> Option<u32> {
        let value = value?;
        if let Some(&index) = self.string_lookup.get(value) {
            return Some(index);
        }
        let index = self.strings.len() as u32;
        self.strings.push(value.clone());
        self.string_lookup.insert(value.clone(), index);
        Some(index)
    }

    /// Appends the record of the next edge. `junction` is the motorway
    /// junction node the edge leaves, if any; its exit number and `exit_to`
    /// apply to link roads only, while its name applies to every branch.
    pub(crate) fn push_edge(
        &mut self,
        way: Option<&WayGuidanceTags>,
        forward: bool,
        highway: HighwayClass,
        junction: Option<&JunctionNodeTags>,
    ) {
        let mut record = EdgeGuidance::default();
        if let Some(way) = way {
            let directional = if forward { &way.forward } else { &way.backward };
            record.reference = self.string(way.reference.as_ref());
            record.turn_lanes = self.string(directional.turn_lanes.as_ref());
            record.destination = self.string(directional.destination.as_ref());
            record.destination_ref = self.string(directional.destination_ref.as_ref());
            record.destination_street = self.string(directional.destination_street.as_ref());
            record.junction_ref = self.string(way.junction_ref.as_ref());
            record.exit_to = self.string(way.exit_to.as_ref());
        }
        if let Some(junction) = junction {
            record.junction_name = self.string(junction.name.as_ref());
            if is_link(highway) {
                if record.junction_ref.is_none() {
                    record.junction_ref = self.string(junction.reference.as_ref());
                }
                if record.exit_to.is_none() {
                    record.exit_to = self.string(junction.exit_to.as_ref());
                }
            }
        }
        let next = self.records.len() as u32;
        let index = *self.record_lookup.entry(record).or_insert(next);
        if index == next {
            self.records.push(record);
        }
        self.edge_records.push(index);
    }

    /// Finishes the bundle. Strings are sorted so the front-coded table
    /// compresses well; record fields are remapped accordingly. A dataset
    /// without any guidance tags gets empty guidance columns.
    pub(crate) fn finish(self, names: Vec<String>) -> EdgeNameBundle {
        if self.records.len() == 1 {
            return EdgeNameBundle::from_names(names);
        }
        let mut order = (0..self.strings.len() as u32).collect::<Vec<_>>();
        order.sort_by(|&a, &b| self.strings[a as usize].cmp(&self.strings[b as usize]));
        let mut remap = vec![0_u32; order.len()];
        for (sorted, &original) in order.iter().enumerate() {
            remap[original as usize] = sorted as u32;
        }
        let map = |index: Option<u32>| index.map(|index| remap[index as usize]);
        let guidance_records = self
            .records
            .into_iter()
            .map(|record| EdgeGuidance {
                reference: map(record.reference),
                turn_lanes: map(record.turn_lanes),
                destination: map(record.destination),
                destination_ref: map(record.destination_ref),
                destination_street: map(record.destination_street),
                junction_ref: map(record.junction_ref),
                junction_name: map(record.junction_name),
                exit_to: map(record.exit_to),
            })
            .collect();
        let mut strings = self.strings;
        strings.sort_unstable();
        EdgeNameBundle {
            names,
            strings,
            guidance_records,
            edge_guidance: self.edge_records,
        }
    }
}

#[cfg(test)]
mod tests;
