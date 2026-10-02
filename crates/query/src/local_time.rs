//! Local wall-clock time on the street network.
//!
//! Every topology node carries the IANA timezone assigned at import. Edge
//! rules written in local time (opening hours, conditional restrictions,
//! time-dependent speeds) are evaluated in the zone of the edge's tail node,
//! so a route that crosses a timezone border switches clocks with the
//! network. Instants are never shifted: only their wall-clock reading
//! changes.
//!
//! Request datetimes without a UTC offset are read in the zone at the
//! request's anchor point. The daylight-saving policy mirrors GTFS requests
//! (see `docs/transit-time.md`): a local time that occurs twice (autumn) is
//! ambiguous and rejected, and one skipped by a spring change does not exist
//! and is rejected; supply an explicit offset for either.

use anyhow::{Context, Result, bail};
use chrono::{LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone};
use chrono_tz::Tz;
use netweevil_core::TopologyBundle;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

/// Parsed timezones of a topology, indexed like `regions.timezones`.
#[derive(Debug, Clone, Default)]
pub struct NetworkTimezones {
    zones: Vec<Option<Tz>>,
}

impl NetworkTimezones {
    /// Parses the dataset's zone names. Names the bundled IANA database
    /// does not know (a newer zone than this build) stay `None`, so their
    /// edges keep the caller's clock.
    pub fn new(topology: &TopologyBundle) -> Self {
        Self {
            zones: topology
                .regions
                .timezones
                .iter()
                .map(|name| name.parse::<Tz>().ok())
                .collect(),
        }
    }

    pub fn node_zone(&self, topology: &TopologyBundle, node_index: usize) -> Option<Tz> {
        let index = *topology.regions.node_timezone.get(node_index)?;
        self.zones.get(index as usize).copied().flatten()
    }

    /// Zone of an edge's tail node.
    pub fn edge_zone(&self, topology: &TopologyBundle, edge_index: usize) -> Option<Tz> {
        let from = topology.edge_layers.routing.get(edge_index)?.from.0 as usize;
        self.node_zone(topology, from)
    }

    /// `instant` expressed in the local offset of `edge_index`; unchanged
    /// where the edge has no zone.
    pub fn edge_local_time(
        &self,
        topology: &TopologyBundle,
        edge_index: usize,
        instant: OffsetDateTime,
    ) -> OffsetDateTime {
        match self.edge_zone(topology, edge_index) {
            Some(zone) => in_zone(instant, zone),
            None => instant,
        }
    }

    /// The single zone of a dataset whose nodes all share one, if any.
    fn single_zone(&self, topology: &TopologyBundle) -> Option<Tz> {
        let first = *topology.regions.node_timezone.first()?;
        topology
            .regions
            .node_timezone
            .iter()
            .all(|index| *index == first)
            .then(|| self.zones.get(first as usize).copied().flatten())
            .flatten()
    }

    /// Zone at a position: the dataset's only zone, or that of the nearest
    /// node.
    pub fn zone_at(&self, topology: &TopologyBundle, lon: f64, lat: f64) -> Option<Tz> {
        if let Some(zone) = self.single_zone(topology) {
            return Some(zone);
        }
        let node = nearest_node(topology, lon, lat)?;
        self.node_zone(topology, node)
    }
}

/// The same instant in `zone`'s offset at that instant.
pub fn in_zone(instant: OffsetDateTime, zone: Tz) -> OffsetDateTime {
    let Some(utc) = chrono::DateTime::from_timestamp(instant.unix_timestamp(), 0) else {
        return instant;
    };
    let seconds = zone
        .offset_from_utc_datetime(&utc.naive_utc())
        .fix()
        .local_minus_utc();
    UtcOffset::from_whole_seconds(seconds).map_or(instant, |offset| instant.to_offset(offset))
}

/// Parses a request datetime: RFC 3339 with an offset, or a local
/// `YYYY-MM-DDTHH:MM[:SS]` (or `YYYY-MM-DD`, meaning midnight) read in
/// `zone`. Ambiguous and nonexistent local times are rejected.
pub fn parse_local_datetime(raw: &str, zone: Option<Tz>) -> Result<OffsetDateTime> {
    let raw = raw.trim();
    if let Ok(datetime) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(datetime);
    }
    let naive = parse_naive_datetime(raw)?;
    let Some(zone) = zone else {
        bail!(
            "datetime '{raw}' has no UTC offset and the dataset has no timezone there; use RFC 3339 with an explicit offset"
        );
    };
    let local = match zone.from_local_datetime(&naive) {
        LocalResult::Single(datetime) => datetime,
        LocalResult::Ambiguous(_, _) => bail!(
            "datetime '{raw}' is ambiguous in {zone} because clocks go back; supply an explicit UTC offset"
        ),
        LocalResult::None => bail!(
            "datetime '{raw}' does not exist in {zone} because clocks go forward; supply an explicit UTC offset"
        ),
    };
    let offset = UtcOffset::from_whole_seconds(local.offset().fix().local_minus_utc())
        .context("timezone offset out of range")?;
    Ok(OffsetDateTime::from_unix_timestamp(local.timestamp())
        .context("datetime out of range")?
        .to_offset(offset))
}

fn parse_naive_datetime(raw: &str) -> Result<NaiveDateTime> {
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).expect("valid midnight"));
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(raw, format) {
            return Ok(datetime);
        }
    }
    bail!(
        "invalid datetime '{raw}'; use RFC 3339 with an offset or a local YYYY-MM-DDTHH:MM:SS datetime"
    )
}

/// Nearest node by the spatial grid, widening the searched ring of cells
/// until a node is found; a linear scan without a grid.
fn nearest_node(topology: &TopologyBundle, lon: f64, lat: f64) -> Option<usize> {
    let distance = |node: usize| {
        let node = &topology.nodes[node];
        netweevil_core::geo::haversine_meters(lon, lat, node.lon, node.lat)
    };
    let Some(index) = topology.spatial_index.as_ref() else {
        return (0..topology.nodes.len()).min_by(|a, b| distance(*a).total_cmp(&distance(*b)));
    };
    let column = (((lon - index.bounds.min_lon) / index.cell_width_deg).floor() as i64)
        .clamp(0, i64::from(index.columns) - 1);
    let row = (((lat - index.bounds.min_lat) / index.cell_height_deg).floor() as i64)
        .clamp(0, i64::from(index.rows) - 1);
    let max_ring = i64::from(index.columns.max(index.rows));
    for ring in 0..=max_ring {
        let mut best: Option<(usize, f64)> = None;
        for cell_row in (row - ring).max(0)..=(row + ring).min(i64::from(index.rows) - 1) {
            for cell_column in
                (column - ring).max(0)..=(column + ring).min(i64::from(index.columns) - 1)
            {
                let on_ring =
                    (cell_row - row).abs() == ring || (cell_column - column).abs() == ring;
                if !on_ring {
                    continue;
                }
                let cell =
                    index.cells[(cell_row * i64::from(index.columns) + cell_column) as usize];
                let start = cell.node_start as usize;
                for &node in &index.node_ids[start..start + cell.node_len as usize] {
                    let candidate = distance(node as usize);
                    if best.is_none_or(|(_, known)| candidate < known) {
                        best = Some((node as usize, candidate));
                    }
                }
            }
        }
        if let Some((node, _)) = best {
            return Some(node);
        }
    }
    None
}

#[cfg(test)]
mod tests;
