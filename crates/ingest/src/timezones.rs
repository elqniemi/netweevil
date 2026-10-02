//! IANA timezone of every topology node, from the timezone boundary data
//! embedded in `tzf-rs` (timezone-boundary-builder polygons, simplified).
//! No network access or system tz database is needed. Points at sea get the
//! nautical `Etc/GMT±N` zones the data set defines.

use std::collections::BTreeMap;

use netweevil_core::{NO_TIMEZONE, TopologyNode};
use rayon::prelude::*;
use tzf_rs::DefaultFinder;

/// Interned zone names and the per-node index into them.
pub(crate) fn assign_timezones(nodes: &[TopologyNode]) -> (Vec<String>, Vec<u32>) {
    let finder = DefaultFinder::new();
    let names = nodes
        .par_iter()
        .map(|node| finder.get_tz_name(node.lon, node.lat))
        .collect::<Vec<_>>();
    let mut zones = Vec::new();
    let mut lookup = BTreeMap::new();
    let indexes = names
        .into_iter()
        .map(|name| {
            if name.is_empty() {
                return NO_TIMEZONE;
            }
            *lookup.entry(name).or_insert_with(|| {
                zones.push(name.to_string());
                (zones.len() - 1) as u32
            })
        })
        .collect();
    (zones, indexes)
}

#[cfg(test)]
mod tests;
