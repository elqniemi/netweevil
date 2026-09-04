use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{
    AccessMode, TRANSIT_STOP_BINDING_SCHEMA_VERSION, TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION,
    TransitBundle, TransitNetworkTransfer, TransitStopBinding, TransitStopBindingSummary,
    TransitStopBindingTable, TransitStopBindingTarget, TransitStreetPath,
    TransitTransferBuildOptions, TransitTransferTable,
};
use crate::runtime::{StopCandidate, StopSpatialIndex, StreetTimeEstimator};

pub fn load_transit_stop_bindings(path: impl AsRef<Path>) -> Result<TransitStopBindingTable> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading transit stop bindings {}", path.display()))?;
    let raw = raw.trim_start_matches('\u{feff}');
    let table = match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("csv") => parse_stop_binding_csv(raw)?,
        Some(extension) if extension.eq_ignore_ascii_case("json") => {
            #[derive(Deserialize)]
            #[serde(untagged)]
            enum BindingDocument {
                Table(TransitStopBindingTable),
                Bindings(Vec<TransitStopBinding>),
            }
            match serde_json::from_str::<BindingDocument>(raw)
                .context("parsing JSON transit stop bindings")?
            {
                BindingDocument::Table(table) => table,
                BindingDocument::Bindings(bindings) => TransitStopBindingTable {
                    schema_version: TRANSIT_STOP_BINDING_SCHEMA_VERSION,
                    feed_id: None,
                    bindings,
                },
            }
        }
        other => bail!(
            "unsupported transit stop binding extension {:?}; use .json or .csv",
            other
        ),
    };
    validate_stop_binding_table(&table)?;
    Ok(table)
}

/// Atomically replaces the bundle's stop bindings with the supplied table.
/// Unknown stops or invalid targets leave the existing bundle unchanged.
pub fn apply_transit_stop_bindings(
    bundle: &mut TransitBundle,
    table: &TransitStopBindingTable,
) -> Result<TransitStopBindingSummary> {
    validate_stop_binding_table(table)?;
    if let Some(feed_id) = table.feed_id.as_deref()
        && feed_id != bundle.feed_id
    {
        bail!(
            "stop binding feed_id '{}' does not match transit bundle '{}'",
            feed_id,
            bundle.feed_id
        );
    }

    let stop_by_id = bundle
        .stops
        .iter()
        .enumerate()
        .map(|(index, stop)| (stop.stop_id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut unknown = Vec::new();
    let mut assignments = Vec::with_capacity(table.bindings.len());
    for binding in &table.bindings {
        let Some(&stop_index) = stop_by_id.get(&binding.stop_id) else {
            unknown.push(binding.stop_id.clone());
            continue;
        };
        assignments.push((stop_index, binding.target.clone()));
    }
    if !unknown.is_empty() {
        unknown.sort();
        bail!(
            "stop binding table contains {} unknown stop_id value(s): {}",
            unknown.len(),
            unknown.into_iter().take(10).collect::<Vec<_>>().join(", ")
        );
    }
    for stop in &mut bundle.stops {
        stop.binding = None;
    }
    for (stop_index, target) in assignments {
        bundle.stops[stop_index].binding = Some(target);
    }
    bundle.stop_binding_sha256 = Some(stop_binding_fingerprint(table)?);
    let bound_stop_count = bundle
        .stops
        .iter()
        .filter(|stop| stop.binding.is_some())
        .count();
    Ok(TransitStopBindingSummary {
        feed_id: bundle.feed_id.clone(),
        supplied_binding_count: table.bindings.len(),
        bound_stop_count,
        unbound_stop_count: bundle.stops.len().saturating_sub(bound_stop_count),
    })
}

fn stop_binding_fingerprint(table: &TransitStopBindingTable) -> Result<String> {
    let mut canonical = table.clone();
    canonical
        .bindings
        .sort_by(|left, right| left.stop_id.cmp(&right.stop_id));
    let bytes = serde_json::to_vec(&canonical)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_stop_binding_table(table: &TransitStopBindingTable) -> Result<()> {
    if table.schema_version != TRANSIT_STOP_BINDING_SCHEMA_VERSION {
        bail!(
            "unsupported transit stop binding schema version {}; expected {}",
            table.schema_version,
            TRANSIT_STOP_BINDING_SCHEMA_VERSION
        );
    }
    let mut seen = HashSet::new();
    for binding in &table.bindings {
        if binding.stop_id.trim().is_empty() {
            bail!("transit stop binding has an empty stop_id");
        }
        if !seen.insert(binding.stop_id.as_str()) {
            bail!("duplicate transit stop binding for '{}'", binding.stop_id);
        }
        validate_binding_target(&binding.stop_id, &binding.target)?;
    }
    Ok(())
}

fn validate_binding_target(stop_id: &str, target: &TransitStopBindingTarget) -> Result<()> {
    match target {
        TransitStopBindingTarget::Node { .. } => {}
        TransitStopBindingTarget::Edge { fraction, .. } => {
            if fraction
                .is_some_and(|fraction| !fraction.is_finite() || !(0.0..=1.0).contains(&fraction))
            {
                bail!("edge binding for stop '{stop_id}' has fraction outside [0, 1]");
            }
        }
        TransitStopBindingTarget::Coordinate {
            lon,
            lat,
            z,
            z_window_m,
            ..
        } => {
            if !lon.is_finite()
                || !lat.is_finite()
                || z.is_some_and(|z| !z.is_finite())
                || z_window_m.is_some_and(|window| !window.is_finite() || window < 0.0)
            {
                bail!("coordinate binding for stop '{stop_id}' contains a non-finite value");
            }
            if !(-180.0..=180.0).contains(lon) || !(-90.0..=90.0).contains(lat) {
                bail!("coordinate binding for stop '{stop_id}' is outside WGS84 bounds");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct CsvStopBinding {
    stop_id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    node_id: Option<u32>,
    #[serde(default)]
    edge_id: Option<u32>,
    #[serde(default)]
    fraction: Option<f64>,
    #[serde(default)]
    lon: Option<f64>,
    #[serde(default)]
    lat: Option<f64>,
    #[serde(default)]
    z: Option<f64>,
    #[serde(default)]
    z_window_m: Option<f64>,
    #[serde(default)]
    attribute_filter: String,
}

fn parse_stop_binding_csv(raw: &str) -> Result<TransitStopBindingTable> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let mut bindings = Vec::new();
    for row in reader.deserialize::<CsvStopBinding>() {
        let row = row.context("parsing CSV transit stop binding")?;
        let inferred_kind = if row.node_id.is_some() {
            "node"
        } else if row.edge_id.is_some() {
            "edge"
        } else if row.lon.is_some()
            || row.lat.is_some()
            || row.z.is_some()
            || row.z_window_m.is_some()
        {
            "coordinate"
        } else {
            ""
        };
        let kind = if row.kind.trim().is_empty() {
            inferred_kind
        } else {
            row.kind.trim()
        };
        let target = match kind {
            "node" => TransitStopBindingTarget::Node {
                node_id: row
                    .node_id
                    .ok_or_else(|| anyhow!("node binding '{}' is missing node_id", row.stop_id))?,
            },
            "edge" => TransitStopBindingTarget::Edge {
                edge_id: row
                    .edge_id
                    .ok_or_else(|| anyhow!("edge binding '{}' is missing edge_id", row.stop_id))?,
                fraction: row.fraction,
            },
            "coordinate" => TransitStopBindingTarget::Coordinate {
                lon: row.lon.ok_or_else(|| {
                    anyhow!("coordinate binding '{}' is missing lon", row.stop_id)
                })?,
                lat: row.lat.ok_or_else(|| {
                    anyhow!("coordinate binding '{}' is missing lat", row.stop_id)
                })?,
                z: row.z,
                z_window_m: row.z_window_m,
                attribute_filter: if row.attribute_filter.trim().is_empty() {
                    BTreeMap::new()
                } else {
                    serde_json::from_str(&row.attribute_filter).with_context(|| {
                        format!(
                            "parsing attribute_filter for transit stop binding '{}'",
                            row.stop_id
                        )
                    })?
                },
            },
            other => bail!(
                "transit stop binding '{}' has unsupported kind '{}'",
                row.stop_id,
                other
            ),
        };
        bindings.push(TransitStopBinding {
            stop_id: row.stop_id,
            target,
        });
    }
    Ok(TransitStopBindingTable {
        schema_version: TRANSIT_STOP_BINDING_SCHEMA_VERSION,
        feed_id: None,
        bindings,
    })
}

pub fn build_transit_transfer_table(
    bundle: &TransitBundle,
    options: &TransitTransferBuildOptions,
    estimator: &dyn StreetTimeEstimator,
) -> Result<TransitTransferTable> {
    if options.profile_id.trim().is_empty() {
        bail!("transfer profile_id must not be empty");
    }
    if options.dataset_id.trim().is_empty() || options.profile_hash.trim().is_empty() {
        bail!("transfer dataset_id and profile_hash must not be empty");
    }
    if !options.max_transfer_distance_m.is_finite() || options.max_transfer_distance_m <= 0.0 {
        bail!("max_transfer_distance_m must be a positive finite value");
    }
    if options.max_candidates_per_stop == 0 {
        bail!("max_candidates_per_stop must be greater than zero");
    }

    let spatial_index = StopSpatialIndex::new(&bundle.stops);
    let transfers_by_stop = bundle
        .stops
        .par_iter()
        .enumerate()
        .map(
            |(from_index, from)| -> Result<Vec<TransitNetworkTransfer>> {
                let mut candidates = spatial_index.nearby_stops(
                    bundle,
                    from.lon,
                    from.lat,
                    options.max_transfer_distance_m,
                );
                candidates.retain(|candidate| candidate.stop_index as usize != from_index);
                candidates.sort_by(|left, right| {
                    left.distance_m
                        .total_cmp(&right.distance_m)
                        .then_with(|| left.stop_index.cmp(&right.stop_index))
                });
                candidates.truncate(options.max_candidates_per_stop);
                let mut transfers = Vec::new();
                for candidate in candidates {
                    let to = &bundle.stops[candidate.stop_index as usize];
                    let Some(path) = estimator.stop_to_stop_path(AccessMode::Walk, from, to) else {
                        continue;
                    };
                    validate_network_path(from.stop_id.as_str(), to.stop_id.as_str(), &path)?;
                    transfers.push(TransitNetworkTransfer {
                        from_stop_id: from.stop_id.clone(),
                        to_stop_id: to.stop_id.clone(),
                        straight_line_distance_m: candidate.distance_m,
                        path,
                    });
                }
                Ok(transfers)
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let mut transfers = transfers_by_stop.into_iter().flatten().collect::<Vec<_>>();
    transfers.sort_by(|left, right| {
        left.from_stop_id
            .cmp(&right.from_stop_id)
            .then_with(|| {
                left.straight_line_distance_m
                    .total_cmp(&right.straight_line_distance_m)
            })
            .then_with(|| left.to_stop_id.cmp(&right.to_stop_id))
    });
    Ok(TransitTransferTable {
        schema_version: TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION,
        feed_id: bundle.feed_id.clone(),
        source_sha256: bundle.source_sha256.clone(),
        stop_binding_sha256: bundle.stop_binding_sha256.clone(),
        dataset_id: options.dataset_id.clone(),
        profile_id: options.profile_id.clone(),
        profile_hash: options.profile_hash.clone(),
        max_transfer_distance_m: options.max_transfer_distance_m,
        transfers,
    })
}

pub fn write_transit_transfer_table(
    path: impl AsRef<Path>,
    table: &TransitTransferTable,
) -> Result<()> {
    validate_transfer_table_header(table)?;
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        let raw = serde_json::to_string_pretty(table)?;
        return fs::write(path, raw).with_context(|| format!("writing {}", path.display()));
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    bincode::serialize_into(file, table)
        .with_context(|| format!("serializing transit transfer table {}", path.display()))
}

pub fn read_transit_transfer_table(path: impl AsRef<Path>) -> Result<TransitTransferTable> {
    let path = path.as_ref();
    let table = if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(raw.trim_start_matches('\u{feff}'))
            .with_context(|| format!("parsing transit transfer table {}", path.display()))?
    } else {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        bincode::deserialize(&bytes)
            .with_context(|| format!("parsing transit transfer table {}", path.display()))?
    };
    validate_transfer_table_header(&table)?;
    Ok(table)
}

fn validate_network_path(
    from_stop_id: &str,
    to_stop_id: &str,
    path: &TransitStreetPath,
) -> Result<()> {
    if path
        .distance_m
        .is_some_and(|distance| !distance.is_finite() || distance < 0.0)
    {
        bail!(
            "network transfer '{} -> {}' has an invalid distance",
            from_stop_id,
            to_stop_id
        );
    }
    if path
        .geometry
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        bail!(
            "network transfer '{} -> {}' has non-finite geometry",
            from_stop_id,
            to_stop_id
        );
    }
    if path.components.values().any(|value| !value.is_finite()) {
        bail!(
            "network transfer '{} -> {}' has a non-finite component",
            from_stop_id,
            to_stop_id
        );
    }
    Ok(())
}

fn validate_transfer_table_header(table: &TransitTransferTable) -> Result<()> {
    if table.schema_version != TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION {
        bail!(
            "unsupported transit transfer table schema version {}; expected {}",
            table.schema_version,
            TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION
        );
    }
    if table.feed_id.trim().is_empty()
        || table.dataset_id.trim().is_empty()
        || table.profile_id.trim().is_empty()
        || table.profile_hash.trim().is_empty()
    {
        bail!(
            "transit transfer table feed_id, dataset_id, profile_id, and profile_hash must not be empty"
        );
    }
    if !table.max_transfer_distance_m.is_finite() || table.max_transfer_distance_m <= 0.0 {
        bail!("transit transfer table has invalid max_transfer_distance_m");
    }
    Ok(())
}

pub(crate) fn index_transit_transfer_table(
    bundle: &TransitBundle,
    table: &TransitTransferTable,
) -> Result<Vec<Vec<StopCandidate>>> {
    validate_transfer_table_header(table)?;
    if table.feed_id != bundle.feed_id {
        bail!(
            "transfer table feed_id '{}' does not match transit bundle '{}'",
            table.feed_id,
            bundle.feed_id
        );
    }
    if table.source_sha256 != bundle.source_sha256 {
        bail!(
            "transfer table source hash does not match transit bundle '{}'; rebuild the table",
            bundle.feed_id
        );
    }
    if table.stop_binding_sha256 != bundle.stop_binding_sha256 {
        bail!(
            "transfer table stop-binding fingerprint does not match transit bundle '{}'; rebuild the table",
            bundle.feed_id
        );
    }
    let stop_by_id = bundle
        .stops
        .iter()
        .enumerate()
        .map(|(index, stop)| (stop.stop_id.as_str(), index as u32))
        .collect::<HashMap<_, _>>();
    let mut indexed = vec![Vec::<StopCandidate>::new(); bundle.stops.len()];
    let mut seen = HashSet::new();
    for transfer in &table.transfers {
        let from_index = *stop_by_id
            .get(transfer.from_stop_id.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "transfer table references unknown from_stop_id '{}'",
                    transfer.from_stop_id
                )
            })?;
        let to_index = *stop_by_id
            .get(transfer.to_stop_id.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "transfer table references unknown to_stop_id '{}'",
                    transfer.to_stop_id
                )
            })?;
        if from_index == to_index {
            bail!(
                "transfer table contains a self-transfer for '{}'",
                transfer.from_stop_id
            );
        }
        if !seen.insert((from_index, to_index)) {
            bail!(
                "transfer table contains duplicate transfer '{} -> {}'",
                transfer.from_stop_id,
                transfer.to_stop_id
            );
        }
        if !transfer.straight_line_distance_m.is_finite() || transfer.straight_line_distance_m < 0.0
        {
            bail!(
                "transfer table entry '{} -> {}' has invalid straight-line distance",
                transfer.from_stop_id,
                transfer.to_stop_id
            );
        }
        validate_network_path(
            transfer.from_stop_id.as_str(),
            transfer.to_stop_id.as_str(),
            &transfer.path,
        )?;
        indexed[from_index as usize].push(StopCandidate {
            stop_index: to_index,
            distance_m: transfer.straight_line_distance_m,
            transfer_time_s: Some(transfer.path.travel_time_s),
            network_path: Some(transfer.path.clone()),
        });
    }
    for candidates in &mut indexed {
        candidates.sort_by(|left, right| {
            left.distance_m
                .total_cmp(&right.distance_m)
                .then_with(|| left.stop_index.cmp(&right.stop_index))
        });
    }
    Ok(indexed)
}

/// Convenience for hosts constructing a table entry directly from a path.
pub fn network_transfer(
    from_stop_id: impl Into<String>,
    to_stop_id: impl Into<String>,
    straight_line_distance_m: f64,
    path: TransitStreetPath,
) -> TransitNetworkTransfer {
    TransitNetworkTransfer {
        from_stop_id: from_stop_id.into(),
        to_stop_id: to_stop_id.into(),
        straight_line_distance_m,
        path,
    }
}
