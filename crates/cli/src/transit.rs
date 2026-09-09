use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use netweevil_core::{CompiledProfileBundle, TopologyBundle, TravelMode};
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_topology_bundle, write_json,
};
use netweevil_profile::{ProfileDocument, ReturnConfig, ReturnGeometry, load_profile};
use netweevil_query::{
    LabeledPoint, PreparedRoutingEngine, RouteRequest, SnapOptions, SnappedPoint,
};
use netweevil_transit::{
    PreparedTransitRouter, StreetTimeEstimator, TransitBundle, TransitFeedManifest,
    TransitImportOptions, TransitStop, TransitStopBindingTarget, TransitStreetPath,
    TransitTransferBuildOptions, TransitTransferTableManifest, apply_transit_stop_bindings,
    build_transit_transfer_table, import_gtfs, load_transit_stop_bindings, read_transit_bundle,
    read_transit_transfer_table, transit_import_summary, write_transit_bundle,
    write_transit_transfer_table,
};

#[derive(Subcommand, Debug)]
pub(crate) enum TransitCommand {
    Import(TransitImportArgs),
    /// Apply or replace exact stop-to-network bindings.
    Bindings {
        #[command(subcommand)]
        command: TransitBindingsCommand,
    },
    /// Build profile-specific stop-to-stop network transfer tables.
    Transfers {
        #[command(subcommand)]
        command: TransitTransfersCommand,
    },
    List,
}

#[derive(Subcommand, Debug)]
pub(crate) enum TransitBindingsCommand {
    Apply(TransitBindingsApplyArgs),
}

#[derive(Subcommand, Debug)]
pub(crate) enum TransitTransfersCommand {
    Build(TransitTransfersBuildArgs),
}

#[derive(Args, Debug)]
pub(crate) struct TransitImportArgs {
    source: PathBuf,
    #[arg(long)]
    name: String,
    #[arg(long)]
    service_start: String,
    #[arg(long, default_value_t = 7)]
    service_days: u32,
    /// Optional JSON/CSV stop binding table applied before the bundle is
    /// written. The same operation is available later through
    /// `transit bindings apply`.
    #[arg(long)]
    stop_bindings: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct TransitBindingsApplyArgs {
    /// Imported transit feed identifier.
    #[arg(long)]
    feed: String,
    /// JSON or CSV binding table.
    bindings: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct TransitTransfersBuildArgs {
    /// Imported transit feed identifier.
    #[arg(long)]
    feed: String,
    /// Street/indoor dataset containing the bound stops.
    #[arg(long)]
    dataset: String,
    /// Compiled foot profile used to route station transfers.
    #[arg(long)]
    profile: PathBuf,
    /// Maximum straight-line stop separation considered for transfer routing.
    #[arg(long, default_value_t = 500.0)]
    max_transfer_distance_m: f64,
    /// Maximum nearest candidate stops routed from each stop.
    #[arg(long, default_value_t = 32)]
    max_candidates_per_stop: usize,
    /// Optional output path. `.json` produces readable JSON; other extensions
    /// use compact bincode. The generated table is registered on the feed.
    #[arg(long)]
    out: Option<PathBuf>,
}

pub(crate) fn transit_import(paths: &WorkspacePaths, args: TransitImportArgs) -> Result<()> {
    let mut bundle = import_gtfs(
        &args.source,
        TransitImportOptions {
            name: args.name.clone(),
            source_label: args.source.display().to_string(),
            service_start_date: args.service_start.clone(),
            service_days: args.service_days,
        },
    )
    .with_context(|| format!("importing GTFS feed {}", args.source.display()))?;
    let binding_summary = if let Some(path) = args.stop_bindings.as_deref() {
        let table = load_transit_stop_bindings(path)?;
        Some(apply_transit_stop_bindings(&mut bundle, &table)?)
    } else {
        None
    };
    let summary = transit_import_summary(&bundle);
    let bundle_path = transit_bundle_path(paths, &args.name, &summary.source_sha256, &bundle);
    write_transit_bundle(&bundle_path, &bundle)?;
    let manifest = TransitFeedManifest {
        feed_id: args.name.clone(),
        label: format!("GTFS transit feed {}", args.name),
        source_path: args.source.display().to_string(),
        source_sha256: summary.source_sha256.clone(),
        imported_at: netweevil_manifest::now_rfc3339()?,
        service_start_date: args.service_start,
        service_days: args.service_days,
        stop_count: summary.stop_count as u64,
        route_count: summary.route_count as u64,
        trip_count: summary.trip_count as u64,
        connection_count: summary.connection_count as u64,
        bundle_path: bundle_path.display().to_string(),
        stop_binding_source_path: args
            .stop_bindings
            .as_ref()
            .map(|path| path.display().to_string()),
        stop_binding_sha256: bundle.stop_binding_sha256.clone(),
        bound_stop_count: binding_summary
            .as_ref()
            .map_or(0, |summary| summary.bound_stop_count as u64),
        transfer_tables: Vec::new(),
    };
    let manifest_path = paths
        .transit_feeds_dir
        .join(format!("{}.json", manifest.feed_id));
    write_json(&manifest_path, &manifest)?;
    println!(
        "imported transit feed '{}' with {} stops, {} routes, {} trips, {} scheduled connections",
        manifest.feed_id,
        manifest.stop_count,
        manifest.route_count,
        manifest.trip_count,
        manifest.connection_count
    );
    println!("transit bundle written to {}", bundle_path.display());
    if let Some(summary) = binding_summary {
        println!(
            "applied {} stop bindings ({} stops remain unbound)",
            summary.bound_stop_count, summary.unbound_stop_count
        );
    }
    println!(
        "transit feed manifest written to {}",
        manifest_path.display()
    );
    Ok(())
}

pub(crate) fn transit_apply_bindings(
    paths: &WorkspacePaths,
    args: TransitBindingsApplyArgs,
) -> Result<()> {
    let mut manifest = read_transit_manifest(paths, &args.feed)?;
    let mut bundle = read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?;
    let table = load_transit_stop_bindings(&args.bindings)?;
    let summary = apply_transit_stop_bindings(&mut bundle, &table)?;
    let bundle_path =
        transit_bundle_path(paths, &manifest.feed_id, &manifest.source_sha256, &bundle);
    write_transit_bundle(&bundle_path, &bundle)?;

    let invalidated_transfer_count = manifest.transfer_tables.len();
    manifest.bundle_path = bundle_path.display().to_string();
    manifest.stop_binding_source_path = Some(args.bindings.display().to_string());
    manifest.stop_binding_sha256 = bundle.stop_binding_sha256.clone();
    manifest.bound_stop_count = summary.bound_stop_count as u64;
    manifest.transfer_tables.clear();
    write_transit_manifest(paths, &manifest)?;

    println!(
        "applied {} stop bindings to feed '{}' ({} stops remain unbound)",
        summary.bound_stop_count, manifest.feed_id, summary.unbound_stop_count
    );
    println!(
        "updated transit bundle written to {}",
        bundle_path.display()
    );
    if invalidated_transfer_count > 0 {
        println!(
            "removed {} stale transfer-table registration(s); rebuild them against the new bindings",
            invalidated_transfer_count
        );
    }
    Ok(())
}

pub(crate) fn transit_build_transfers(
    paths: &WorkspacePaths,
    args: TransitTransfersBuildArgs,
) -> Result<()> {
    let mut manifest = read_transit_manifest(paths, &args.feed)?;
    let bundle = read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?;
    let profile = load_profile(&args.profile)
        .with_context(|| format!("loading transfer profile {}", args.profile.display()))?;
    profile.validate()?;
    if profile.profile.mode != TravelMode::Foot {
        bail!(
            "transit transfer profile '{}' uses mode {:?}; a foot profile is required",
            profile.profile.id,
            profile.profile.mode
        );
    }
    let (profile_hash, engine) = load_transfer_routing_engine(paths, &args.dataset, &profile)?;
    let estimator = BoundStopTransferEstimator::new(Arc::new(engine), &bundle)?;
    let table = build_transit_transfer_table(
        &bundle,
        &TransitTransferBuildOptions {
            dataset_id: args.dataset.clone(),
            profile_id: profile.profile.id.clone(),
            profile_hash: profile_hash.clone(),
            max_transfer_distance_m: args.max_transfer_distance_m,
            max_candidates_per_stop: args.max_candidates_per_stop,
        },
        &estimator,
    )?;
    let output_path = args.out.unwrap_or_else(|| {
        paths.transit_bundles_dir.join(format!(
            "transfers-{}-{}-{}-{}.bin",
            manifest.feed_id,
            args.dataset,
            table.profile_id,
            short_hash(&table.profile_hash)
        ))
    });
    write_transit_transfer_table(&output_path, &table)?;

    // Runtime selection is profile-keyed, so a feed may register only one
    // dataset-specific table for a given profile at a time.
    manifest
        .transfer_tables
        .retain(|entry| entry.profile_id != table.profile_id);
    manifest.transfer_tables.push(TransitTransferTableManifest {
        profile_id: table.profile_id.clone(),
        profile_hash: table.profile_hash.clone(),
        dataset_id: table.dataset_id.clone(),
        path: output_path.display().to_string(),
        created_at: netweevil_manifest::now_rfc3339()?,
        transfer_count: table.transfers.len() as u64,
        max_transfer_distance_m: table.max_transfer_distance_m,
    });
    manifest.transfer_tables.sort_by(|left, right| {
        left.profile_id
            .cmp(&right.profile_id)
            .then_with(|| left.dataset_id.cmp(&right.dataset_id))
    });
    write_transit_manifest(paths, &manifest)?;

    println!(
        "built {} directed network transfers for feed '{}' with profile '{}'",
        table.transfers.len(),
        manifest.feed_id,
        table.profile_id
    );
    println!("transfer table written to {}", output_path.display());
    Ok(())
}

fn transit_bundle_path(
    paths: &WorkspacePaths,
    feed_id: &str,
    source_sha256: &str,
    bundle: &TransitBundle,
) -> PathBuf {
    let binding_suffix = bundle
        .stop_binding_sha256
        .as_deref()
        .map(|hash| format!("-bindings-{}", short_hash(hash)))
        .unwrap_or_default();
    paths.transit_bundles_dir.join(format!(
        "transit-{}-{}{}.bin",
        feed_id,
        short_hash(source_sha256),
        binding_suffix
    ))
}

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn write_transit_manifest(paths: &WorkspacePaths, manifest: &TransitFeedManifest) -> Result<()> {
    let path = paths
        .transit_feeds_dir
        .join(format!("{}.json", manifest.feed_id));
    write_json(path, manifest)
}

pub(crate) fn prepare_registered_transit_router(
    manifest: &TransitFeedManifest,
    bundle: TransitBundle,
) -> Result<PreparedTransitRouter> {
    let tables = manifest
        .transfer_tables
        .iter()
        .map(|entry| {
            read_transit_transfer_table(&entry.path)
                .with_context(|| format!("reading transit transfer table {}", entry.path))
        })
        .collect::<Result<Vec<_>>>()?;
    if tables.is_empty() {
        Ok(PreparedTransitRouter::new(Arc::new(bundle)))
    } else {
        PreparedTransitRouter::new_with_transfer_tables(Arc::new(bundle), tables)
    }
}

fn load_transfer_routing_engine(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
) -> Result<(String, PreparedRoutingEngine)> {
    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .as_ref()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    let acceleration = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
        })
        .transpose()?;
    let profile_hash = profile.fingerprint()?;
    let compiled_manifest = read_compiled_profile_manifests(paths)?
        .into_iter()
        .find(|manifest| {
            manifest.dataset_id.0 == dataset_id && manifest.profile_hash == profile_hash
        })
        .context(
            "compiled profile bundle not found; run `netweevil profile compile` for this dataset and profile first",
        )?;
    let compiled: CompiledProfileBundle =
        read_compiled_profile_bundle(&compiled_manifest.bundle.path).with_context(|| {
            format!(
                "reading compiled profile bundle {}",
                compiled_manifest.bundle.path
            )
        })?;
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled),
        acceleration.map(Arc::new),
    )
    .context("preparing transfer routing engine")?;
    Ok((profile_hash, engine))
}

pub(crate) fn prepare_cli_transit_street_estimator(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile_path: &Path,
    bundle: &TransitBundle,
) -> Result<BoundStopTransferEstimator> {
    let profile = load_profile(profile_path)
        .with_context(|| format!("loading pedestrian profile {}", profile_path.display()))?;
    profile.validate()?;
    if profile.profile.mode != TravelMode::Foot {
        bail!(
            "transit network access profile '{}' uses mode {:?}; a foot profile is required",
            profile.profile.id,
            profile.profile.mode
        );
    }
    let (_, engine) = load_transfer_routing_engine(paths, dataset_id, &profile)?;
    BoundStopTransferEstimator::new(Arc::new(engine), bundle)
}

#[derive(Clone)]
struct ResolvedStopCandidates {
    point: LabeledPoint,
    origins: Vec<SnappedPoint>,
    destinations: Vec<SnappedPoint>,
}

pub(crate) struct BoundStopTransferEstimator {
    engine: Arc<PreparedRoutingEngine>,
    stops: HashMap<String, ResolvedStopCandidates>,
}

impl BoundStopTransferEstimator {
    fn new(engine: Arc<PreparedRoutingEngine>, bundle: &TransitBundle) -> Result<Self> {
        let topology = engine.topology();
        let edge_index_by_id = (0..topology.edge_count())
            .map(|index| (topology.routing_edge(index).edge_id.0, index as u32))
            .collect::<HashMap<_, _>>();
        let mut stops = HashMap::new();
        for stop in &bundle.stops {
            match resolve_stop_candidates(engine.as_ref(), stop, &edge_index_by_id) {
                Ok(Some(candidates)) => {
                    stops.insert(stop.stop_id.clone(), candidates);
                }
                Ok(None) if stop.binding.is_none() => {}
                Ok(None) => bail!(
                    "bound transit stop '{}' could not be resolved on the routing graph",
                    stop.stop_id
                ),
                Err(_) if stop.binding.is_none() => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("resolving bound transit stop '{}'", stop.stop_id)
                    });
                }
            }
        }
        Ok(Self { engine, stops })
    }

    fn route_candidates(
        &self,
        route_id: String,
        from: &ResolvedStopCandidates,
        to: &ResolvedStopCandidates,
    ) -> Option<TransitStreetPath> {
        let request = transfer_route_request(route_id, from.point.clone(), to.point.clone());
        let result = self
            .engine
            .execute_route_between_candidates(&request, &from.origins, &to.destinations)
            .ok()?;
        Some(route_result_to_street_path(result))
    }

    fn point_candidates(&self, id: &str, lon: f64, lat: f64) -> Option<ResolvedStopCandidates> {
        let point = LabeledPoint {
            id: id.to_string(),
            lon,
            lat,
            z: None,
        };
        let mut origins = self
            .engine
            .snap_route_candidates(&point, 500.0, true)
            .ok()?;
        let mut destinations = self
            .engine
            .snap_route_candidates(&point, 500.0, false)
            .ok()?;
        origins.truncate(8);
        destinations.truncate(8);
        (!origins.is_empty() && !destinations.is_empty()).then_some(ResolvedStopCandidates {
            point,
            origins,
            destinations,
        })
    }
}

impl StreetTimeEstimator for BoundStopTransferEstimator {
    fn street_time_s(
        &self,
        _mode: netweevil_transit::AccessMode,
        _egress: bool,
        from_lon: f64,
        from_lat: f64,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<u32> {
        let request = transfer_route_request(
            "transit_transfer_fallback".to_string(),
            LabeledPoint {
                id: "from".to_string(),
                lon: from_lon,
                lat: from_lat,
                z: None,
            },
            LabeledPoint {
                id: "to".to_string(),
                lon: to_lon,
                lat: to_lat,
                z: None,
            },
        );
        self.engine
            .execute_route(&request)
            .ok()
            .map(|route| route_time_s(&route))
    }

    fn point_to_stop_path(
        &self,
        _mode: netweevil_transit::AccessMode,
        from_lon: f64,
        from_lat: f64,
        stop: &TransitStop,
    ) -> Option<TransitStreetPath> {
        self.route_candidates(
            format!("access_{}", stop.stop_id),
            &self.point_candidates("transit_access_origin", from_lon, from_lat)?,
            self.stops.get(&stop.stop_id)?,
        )
    }

    fn stop_to_point_path(
        &self,
        _mode: netweevil_transit::AccessMode,
        stop: &TransitStop,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<TransitStreetPath> {
        self.route_candidates(
            format!("egress_{}", stop.stop_id),
            self.stops.get(&stop.stop_id)?,
            &self.point_candidates("transit_egress_destination", to_lon, to_lat)?,
        )
    }

    fn stop_to_stop_path(
        &self,
        _mode: netweevil_transit::AccessMode,
        from: &TransitStop,
        to: &TransitStop,
    ) -> Option<TransitStreetPath> {
        self.route_candidates(
            format!("transfer_{}_{}", from.stop_id, to.stop_id),
            self.stops.get(&from.stop_id)?,
            self.stops.get(&to.stop_id)?,
        )
    }
}

fn resolve_stop_candidates(
    engine: &PreparedRoutingEngine,
    stop: &TransitStop,
    edge_index_by_id: &HashMap<u32, u32>,
) -> Result<Option<ResolvedStopCandidates>> {
    let topology = engine.topology();
    let point = binding_point(stop);
    let (mut origins, mut destinations) = match stop.binding.as_ref() {
        Some(TransitStopBindingTarget::Node { node_id }) => {
            let candidate = exact_node_candidate(topology, &point, *node_id)?;
            (vec![candidate.clone()], vec![candidate])
        }
        Some(TransitStopBindingTarget::Edge { edge_id, fraction }) => {
            let edge_index = *edge_index_by_id
                .get(edge_id)
                .with_context(|| format!("binding references unknown edge_id {edge_id}"))?;
            let candidate = exact_edge_candidate(
                topology,
                &point,
                edge_index,
                fraction.unwrap_or_else(|| projected_edge_fraction(topology, edge_index, &point)),
            )?;
            (vec![candidate.clone()], vec![candidate])
        }
        Some(TransitStopBindingTarget::Coordinate {
            z,
            z_window_m,
            attribute_filter,
            ..
        }) => {
            let options = SnapOptions {
                max_distance_m: 500.0,
                z_window_m: *z_window_m,
                attribute_filters: attribute_filter.clone(),
            };
            let mut origins = engine.snap_route_candidates_with_options(&point, &options, true)?;
            let mut destinations =
                engine.snap_route_candidates_with_options(&point, &options, false)?;
            if z.is_some() && z_window_m.is_none() {
                retain_nearest_z(&mut origins, z.unwrap_or_default());
                retain_nearest_z(&mut destinations, z.unwrap_or_default());
            }
            (origins, destinations)
        }
        None => (
            engine
                .snap_route_candidates(&point, 500.0, true)
                .ok()
                .unwrap_or_default(),
            engine
                .snap_route_candidates(&point, 500.0, false)
                .ok()
                .unwrap_or_default(),
        ),
    };
    origins.truncate(8);
    destinations.truncate(8);
    if origins.is_empty() || destinations.is_empty() {
        return Ok(None);
    }
    Ok(Some(ResolvedStopCandidates {
        point,
        origins,
        destinations,
    }))
}

fn binding_point(stop: &TransitStop) -> LabeledPoint {
    match stop.binding.as_ref() {
        Some(TransitStopBindingTarget::Coordinate { lon, lat, z, .. }) => LabeledPoint {
            id: stop.stop_id.clone(),
            lon: *lon,
            lat: *lat,
            z: *z,
        },
        _ => LabeledPoint {
            id: stop.stop_id.clone(),
            lon: stop.lon,
            lat: stop.lat,
            z: None,
        },
    }
}

fn exact_node_candidate(
    topology: &TopologyBundle,
    point: &LabeledPoint,
    node_id: u32,
) -> Result<SnappedPoint> {
    let node = topology
        .nodes
        .get(node_id as usize)
        .filter(|node| node.node_id.0 == node_id)
        .with_context(|| format!("binding references unknown node_id {node_id}"))?;
    Ok(SnappedPoint {
        point_id: point.id.clone(),
        requested_lon: point.lon,
        requested_lat: point.lat,
        snapped_node_id: node_id,
        snapped_lon: node.lon,
        snapped_lat: node.lat,
        snapped_z: node.elevation_m().unwrap_or_default(),
        snap_distance_m: 0.0,
        snapped_edge_id: None,
        snapped_edge_fraction: None,
        snapped_from_node_id: None,
        snapped_to_node_id: None,
        component_id: topology.node_component_id(node_id),
    })
}

fn exact_edge_candidate(
    topology: &TopologyBundle,
    point: &LabeledPoint,
    edge_index: u32,
    fraction: f64,
) -> Result<SnappedPoint> {
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        bail!("edge binding fraction must be in [0, 1]");
    }
    let edge = topology.routing_edge(edge_index as usize);
    let from = &topology.nodes[edge.from.0 as usize];
    let to = &topology.nodes[edge.to.0 as usize];
    let z = match (from.elevation_m(), to.elevation_m()) {
        (Some(from), Some(to)) => from + (to - from) * fraction,
        _ => 0.0,
    };
    Ok(SnappedPoint {
        point_id: point.id.clone(),
        requested_lon: point.lon,
        requested_lat: point.lat,
        snapped_node_id: if fraction <= 0.5 {
            edge.from.0
        } else {
            edge.to.0
        },
        snapped_lon: from.lon + (to.lon - from.lon) * fraction,
        snapped_lat: from.lat + (to.lat - from.lat) * fraction,
        snapped_z: z,
        snap_distance_m: 0.0,
        // Query endpoint candidates use the routing edge index here.
        snapped_edge_id: Some(edge_index),
        snapped_edge_fraction: Some(fraction),
        snapped_from_node_id: Some(edge.from.0),
        snapped_to_node_id: Some(edge.to.0),
        component_id: topology.edge_component_id(edge_index),
    })
}

fn projected_edge_fraction(
    topology: &TopologyBundle,
    edge_index: u32,
    point: &LabeledPoint,
) -> f64 {
    let edge = topology.routing_edge(edge_index as usize);
    let from = &topology.nodes[edge.from.0 as usize];
    let to = &topology.nodes[edge.to.0 as usize];
    let cos_lat = from.lat.to_radians().cos().abs().max(0.01);
    let point_x = (point.lon - from.lon) * cos_lat;
    let point_y = point.lat - from.lat;
    let edge_x = (to.lon - from.lon) * cos_lat;
    let edge_y = to.lat - from.lat;
    let length_sq = edge_x.mul_add(edge_x, edge_y * edge_y);
    if length_sq <= f64::EPSILON {
        0.0
    } else {
        ((point_x * edge_x + point_y * edge_y) / length_sq).clamp(0.0, 1.0)
    }
}

fn retain_nearest_z(candidates: &mut Vec<SnappedPoint>, z: f64) {
    if let Some(nearest) = candidates
        .iter()
        .map(|candidate| (candidate.snapped_z - z).abs())
        .min_by(f64::total_cmp)
    {
        candidates.retain(|candidate| (candidate.snapped_z - z).abs() <= nearest + 1.0e-6);
    }
}

fn transfer_route_request(
    route_id: String,
    origin: LabeledPoint,
    destination: LabeledPoint,
) -> RouteRequest {
    RouteRequest {
        route_id,
        origin,
        destination,
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

fn route_time_s(route: &netweevil_query::RouteResult) -> u32 {
    (route.summary.total_travel_time_s.ceil() as u32).max(1)
}

fn route_result_to_street_path(route: netweevil_query::RouteResult) -> TransitStreetPath {
    TransitStreetPath {
        travel_time_s: route_time_s(&route),
        distance_m: Some(route.summary.network_distance_m as f64),
        edge_path: route.edge_path,
        geometry: route.geometry.unwrap_or_default(),
        components: route.summary.components,
    }
}

pub(crate) fn transit_list(paths: &WorkspacePaths) -> Result<()> {
    for manifest in read_transit_manifests(paths)? {
        let transfer_profiles = manifest
            .transfer_tables
            .iter()
            .map(|table| table.profile_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{}\t{}..+{}d\t{} stops ({} bound)\t{} connections\ttransfers [{}]\t{}",
            manifest.feed_id,
            manifest.service_start_date,
            manifest.service_days,
            manifest.stop_count,
            manifest.bound_stop_count,
            manifest.connection_count,
            transfer_profiles,
            manifest.source_path
        );
    }
    Ok(())
}

pub(crate) fn read_transit_manifests(paths: &WorkspacePaths) -> Result<Vec<TransitFeedManifest>> {
    let mut manifests = Vec::new();
    if !paths.transit_feeds_dir.exists() {
        return Ok(manifests);
    }
    for entry in fs::read_dir(&paths.transit_feeds_dir)
        .with_context(|| format!("reading {}", paths.transit_feeds_dir.display()))?
    {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            manifests.push(netweevil_persist::read_json(entry.path())?);
        }
    }
    manifests.sort_by(|left, right| left.feed_id.cmp(&right.feed_id));
    Ok(manifests)
}

pub(crate) fn read_transit_manifest(
    paths: &WorkspacePaths,
    feed_id: &str,
) -> Result<TransitFeedManifest> {
    let path = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
    netweevil_persist::read_json(&path)
        .with_context(|| format!("reading transit feed manifest {}", path.display()))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_transfer_build_command() {
        let cli = crate::Cli::try_parse_from([
            "netweevil",
            "transit",
            "transfers",
            "build",
            "--feed",
            "hk-mtr",
            "--dataset",
            "hk-pedestrian",
            "--profile",
            "step-free.yml",
            "--max-transfer-distance-m",
            "750",
        ])
        .expect("transfer build command parses");
        let crate::Command::Transit {
            command:
                TransitCommand::Transfers {
                    command: TransitTransfersCommand::Build(args),
                },
        } = cli.command
        else {
            panic!("expected transit transfers build command");
        };
        assert_eq!(args.feed, "hk-mtr");
        assert_eq!(args.dataset, "hk-pedestrian");
        assert_eq!(args.max_transfer_distance_m, 750.0);
        assert_eq!(args.max_candidates_per_stop, 32);
    }

    #[test]
    fn parses_binding_apply_command() {
        let cli = crate::Cli::try_parse_from([
            "netweevil",
            "transit",
            "bindings",
            "apply",
            "--feed",
            "hk-mtr",
            "bindings.json",
        ])
        .expect("binding apply command parses");
        let crate::Command::Transit {
            command:
                TransitCommand::Bindings {
                    command: TransitBindingsCommand::Apply(args),
                },
        } = cli.command
        else {
            panic!("expected transit bindings apply command");
        };
        assert_eq!(args.feed, "hk-mtr");
        assert_eq!(args.bindings, PathBuf::from("bindings.json"));
    }
}
