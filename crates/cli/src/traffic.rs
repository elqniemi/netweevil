//! `netweevil traffic`: import historical speed profiles, synthesize an
//! example set, and run traffic-aware routes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use netweevil_core::{RoadClass, TrafficSpeedBundle};
use netweevil_ingest::{
    TrafficSourceDocument, TrafficSynthesisOptions, read_traffic_source, synthesize_traffic_source,
    traffic_binding_stats,
};
use netweevil_persist::manifest::{BundleRef, now_rfc3339};
use netweevil_persist::{
    TrafficManifest, WorkspacePaths, read_dataset_manifest, read_topology_bundle,
    read_traffic_bundle, read_traffic_manifest, read_traffic_manifests, traffic_bundle_path,
    write_json, write_traffic_bundle, write_traffic_manifest,
};
use netweevil_profile::load_profile;
use netweevil_query::{
    LiveTrafficUpdate, PreparedRoutingEngine, TrafficMode, TrafficService, load_route_request,
};
use serde::Serialize;

use crate::analyze::load_route_execution_inputs;

#[derive(Subcommand, Debug)]
pub(crate) enum TrafficCommand {
    /// Import a historical speed source for a dataset.
    ///
    /// The source is a YAML or JSON document with optional CSV files.
    Import(TrafficImportArgs),
    /// List the imported traffic sets.
    List,
    /// Write a deterministic synthetic speed source for a dataset's roads.
    Synthesize(TrafficSynthesizeArgs),
    /// Route with historical and live traffic.
    Route(TrafficRouteArgs),
}

#[derive(Args, Debug)]
pub(crate) struct TrafficImportArgs {
    /// Traffic source document (YAML or JSON).
    source: PathBuf,
    /// Id of an imported dataset.
    #[arg(long)]
    dataset: String,
    /// Traffic set id; defaults to the source's `traffic_id`.
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct TrafficSynthesizeArgs {
    /// Id of an imported dataset.
    #[arg(long)]
    dataset: String,
    /// Traffic set id written into the source.
    #[arg(long)]
    name: String,
    /// Output document (`.yml`); assignments go to `<stem>_assignments.csv`.
    #[arg(long)]
    out: PathBuf,
    /// IANA time zone of the weekly speed patterns.
    #[arg(long, default_value = "Europe/Amsterdam")]
    timezone: String,
    /// Length of one speed bucket in minutes.
    #[arg(long, default_value_t = 15)]
    bucket_minutes: u16,
    /// Road classes that get speed patterns, separated by commas.
    #[arg(long, value_enum, value_delimiter = ',', default_values_t = [
        RoadClassArg::Motorway, RoadClassArg::Trunk, RoadClassArg::Primary,
        RoadClassArg::Secondary,
    ])]
    classes: Vec<RoadClassArg>,
    /// Speed-scale variants per road class.
    #[arg(long, default_value_t = 4)]
    variants: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RoadClassArg {
    Motorway,
    Trunk,
    Primary,
    Secondary,
    Tertiary,
    Residential,
    Service,
}

impl From<RoadClassArg> for RoadClass {
    fn from(value: RoadClassArg) -> Self {
        match value {
            RoadClassArg::Motorway => Self::Motorway,
            RoadClassArg::Trunk => Self::Trunk,
            RoadClassArg::Primary => Self::Primary,
            RoadClassArg::Secondary => Self::Secondary,
            RoadClassArg::Tertiary => Self::Tertiary,
            RoadClassArg::Residential => Self::Residential,
            RoadClassArg::Service => Self::Service,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrafficModeArg {
    Auto,
    Historical,
    TimeDependent,
    Off,
}

impl From<TrafficModeArg> for TrafficMode {
    fn from(value: TrafficModeArg) -> Self {
        match value {
            TrafficModeArg::Auto => Self::Auto,
            TrafficModeArg::Historical => Self::Historical,
            TrafficModeArg::TimeDependent => Self::TimeDependent,
            TrafficModeArg::Off => Self::Off,
        }
    }
}

#[derive(Args, Debug)]
pub(crate) struct TrafficRouteArgs {
    /// Id of an imported dataset.
    #[arg(long)]
    dataset: String,
    /// Routing profile file (YAML or TOML) of a motorized mode.
    #[arg(long)]
    profile: PathBuf,
    /// Imported historical traffic set; omit for live updates only.
    #[arg(long)]
    traffic: Option<String>,
    /// Route request (JSON or YAML).
    #[arg(long)]
    request: PathBuf,
    /// RFC 3339 departure; replaces the request's departure_time/arrive_by.
    #[arg(long)]
    departure_time: Option<String>,
    /// Traffic mode; replaces the `traffic` field of the request.
    #[arg(long, value_enum)]
    mode: Option<TrafficModeArg>,
    /// JSON list of live updates applied before routing.
    #[arg(long)]
    live: Option<PathBuf>,
    /// Receipt time of the live updates (RFC 3339); defaults to now.
    #[arg(long)]
    live_at: Option<String>,
    /// Result file (JSON). Omit it to write to the runs directory.
    #[arg(long)]
    out: Option<PathBuf>,
}

pub(crate) fn traffic_command(paths: &WorkspacePaths, command: TrafficCommand) -> Result<()> {
    match command {
        TrafficCommand::Import(args) => traffic_import(paths, args),
        TrafficCommand::List => traffic_list(paths),
        TrafficCommand::Synthesize(args) => traffic_synthesize(paths, args),
        TrafficCommand::Route(args) => traffic_route(paths, args),
    }
}

fn traffic_import(paths: &WorkspacePaths, args: TrafficImportArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let imported = read_traffic_source(&args.source, args.name.as_deref())?;
    let bundle = imported.bundle;
    let dataset = read_dataset_manifest(paths, &args.dataset)
        .with_context(|| format!("reading dataset manifest for '{}'", args.dataset))?;
    let topology_ref = dataset
        .topology_bundle
        .context("dataset is missing a topology bundle")?;
    let topology = read_topology_bundle(&topology_ref.path)?;
    let stats = traffic_binding_stats(&bundle, &topology);
    if stats.matched_edge_count == 0 {
        bail!(
            "no edge of dataset '{}' matches a feature of traffic set '{}'; check that feature ids are the dataset's source feature (OSM way) ids",
            args.dataset,
            bundle.traffic_id
        );
    }
    let bundle_path = traffic_bundle_path(paths, &bundle.traffic_id);
    write_traffic_bundle(&bundle_path, &bundle)?;
    let bundle_bytes = std::fs::metadata(&bundle_path)?.len();
    let manifest = TrafficManifest {
        traffic_id: bundle.traffic_id.clone(),
        dataset_id: args.dataset.clone(),
        timezone: bundle.timezone.clone(),
        bucket_minutes: bundle.bucket_minutes,
        pattern_count: bundle.pattern_count(),
        feature_count: stats.feature_count,
        matched_feature_count: stats.matched_feature_count,
        matched_edge_count: stats.matched_edge_count,
        source_paths: imported.source_paths,
        created_at: now_rfc3339()?,
        bundle: BundleRef {
            bundle_id: netweevil_core::CacheBundleId::new(format!("traffic-{}", bundle.traffic_id)),
            path: bundle_path.display().to_string(),
        },
    };
    write_traffic_manifest(paths, &manifest)?;
    println!(
        "imported traffic '{}' for dataset '{}': {} patterns ({} min buckets, {}), {} directed features ({} matched, {} edges), bundle {} bytes in {:.2} s",
        manifest.traffic_id,
        manifest.dataset_id,
        manifest.pattern_count,
        manifest.bucket_minutes,
        manifest.timezone,
        manifest.feature_count,
        manifest.matched_feature_count,
        manifest.matched_edge_count,
        bundle_bytes,
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn traffic_list(paths: &WorkspacePaths) -> Result<()> {
    for manifest in read_traffic_manifests(paths)? {
        println!(
            "{}\tdataset={}\tpatterns={}\tfeatures={}\tmatched_edges={}\t{} min {}",
            manifest.traffic_id,
            manifest.dataset_id,
            manifest.pattern_count,
            manifest.feature_count,
            manifest.matched_edge_count,
            manifest.bucket_minutes,
            manifest.timezone,
        );
    }
    Ok(())
}

fn traffic_synthesize(paths: &WorkspacePaths, args: TrafficSynthesizeArgs) -> Result<()> {
    let dataset = read_dataset_manifest(paths, &args.dataset)?;
    let topology_ref = dataset
        .topology_bundle
        .context("dataset is missing a topology bundle")?;
    let topology = read_topology_bundle(&topology_ref.path)?;
    let (mut document, assignments) = synthesize_traffic_source(
        &topology,
        &args.name,
        &TrafficSynthesisOptions {
            timezone: args.timezone,
            bucket_minutes: args.bucket_minutes,
            road_classes: args.classes.into_iter().map(RoadClass::from).collect(),
            variants: args.variants,
        },
    )?;
    let stem = args
        .out
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("--out needs a file name")?;
    let csv_name = format!("{stem}_assignments.csv");
    let csv_path = args.out.with_file_name(&csv_name);
    document.assignments_csv = Some(PathBuf::from(&csv_name));
    if let Some(parent) = args
        .out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.out, compact_traffic_yaml(&document))?;
    std::fs::write(&csv_path, &assignments)?;
    println!(
        "wrote {} ({} patterns) and {} ({} directed features)",
        args.out.display(),
        document.patterns.len(),
        csv_path.display(),
        assignments.lines().count().saturating_sub(1)
    );
    Ok(())
}

/// The document as YAML with one flow sequence per speed profile, three
/// hours of buckets per line.
fn compact_traffic_yaml(document: &TrafficSourceDocument) -> String {
    use std::fmt::Write;
    let mut out = String::from(
        "# Synthetic weekday/weekend congestion by road class for testing traffic-aware\n\
         # routing. Generated by `netweevil traffic synthesize`; not observed data.\n",
    );
    if let Some(id) = &document.traffic_id {
        let _ = writeln!(out, "traffic_id: {id}");
    }
    let _ = writeln!(out, "timezone: {}", document.timezone);
    let _ = writeln!(out, "bucket_minutes: {}", document.bucket_minutes);
    if let Some(csv) = &document.assignments_csv {
        let _ = writeln!(out, "assignments_csv: {}", csv.display());
    }
    let per_line = usize::from((180 / document.bucket_minutes.max(1)).max(1));
    let list = |out: &mut String, key: &str, speeds: &[f64]| {
        let _ = writeln!(out, "    {key}: [");
        for chunk in speeds.chunks(per_line) {
            let values = chunk.iter().map(f64::to_string).collect::<Vec<_>>();
            let _ = writeln!(out, "      {},", values.join(", "));
        }
        let _ = writeln!(out, "    ]");
    };
    out.push_str("patterns:\n");
    for pattern in &document.patterns {
        let _ = writeln!(out, "  - id: {}", pattern.id);
        list(&mut out, "speeds_kph", &pattern.speeds_kph);
        if let Some(weekend) = &pattern.weekend_speeds_kph {
            list(&mut out, "weekend_speeds_kph", weekend);
        }
    }
    out
}

/// Loads the base engine of a compiled profile plus a traffic service for
/// it, with the named historical set (if any).
pub(crate) fn load_traffic_service(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile_path: &Path,
    traffic_id: Option<&str>,
    cache_entries: usize,
) -> Result<(TrafficService, Option<TrafficSpeedBundle>, String)> {
    let profile = load_profile(profile_path)?;
    profile.validate()?;
    let (topology, acceleration, _, compiled) =
        load_route_execution_inputs(paths, dataset_id, &profile)?;
    let historical = traffic_id
        .map(|traffic_id| {
            let manifest = read_traffic_manifest(paths, traffic_id)?;
            read_traffic_bundle(&manifest.bundle.path)
        })
        .transpose()?;
    let topology = Arc::new(topology);
    let base = Arc::new(PreparedRoutingEngine::new(
        topology.clone(),
        Arc::new(compiled),
        acceleration.map(Arc::new),
    )?);
    let profile_id = profile.profile.id.clone();
    let service = TrafficService::new(
        topology,
        historical.as_ref(),
        [(profile_id.clone(), base)],
        cache_entries,
    )?;
    if service.router(&profile_id).is_none() {
        bail!("traffic applies to motorized profiles only; '{profile_id}' is not one");
    }
    Ok((service, historical, profile_id))
}

#[derive(Serialize)]
struct TrafficRouteOutput<'a> {
    traffic: &'a netweevil_query::TrafficSelection,
    result: &'a netweevil_query::RouteResult,
}

fn traffic_route(paths: &WorkspacePaths, args: TrafficRouteArgs) -> Result<()> {
    let (service, _, profile_id) = load_traffic_service(
        paths,
        &args.dataset,
        &args.profile,
        args.traffic.as_deref(),
        4,
    )?;
    if let Some(live) = &args.live {
        let updates: Vec<LiveTrafficUpdate> = serde_json::from_str(
            &std::fs::read_to_string(live)
                .with_context(|| format!("reading {}", live.display()))?,
        )
        .context("parsing live updates")?;
        let now = match &args.live_at {
            Some(value) => netweevil_query::parse_datetime(value)?,
            None => time::OffsetDateTime::now_utc(),
        };
        let report = service.apply_live(&updates, false, now)?;
        println!(
            "applied {} live updates (version {}, {} entries) in {:.1} ms",
            report.update_count, report.version, report.entry_count, report.swap_ms
        );
    }
    let mut request = load_route_request(&args.request)?;
    if let Some(departure) = args.departure_time {
        request.temporal.departure_time = Some(departure);
        request.temporal.arrive_by = None;
    }
    if let Some(mode) = args.mode {
        request.temporal.traffic = mode.into();
    }
    let router = service.router(&profile_id).expect("checked at load");
    let (result, selection) = router.execute_route(&request, None)?.context(
        "traffic does not apply to this request; set departure_time (or arrive_by) and a traffic mode other than off",
    )?;
    let out = args.out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("traffic-route-{}.json", request.route_id))
    });
    write_json(
        &out,
        &TrafficRouteOutput {
            traffic: &selection,
            result: &result,
        },
    )?;
    println!(
        "route '{}' via {}: depart {} arrive {} ({:.0} s, {} m), bucket {}, cache {} ({:.1} ms)",
        result.route_id,
        selection.engine,
        result.summary.departure_time.as_deref().unwrap_or("-"),
        result.summary.arrival_time.as_deref().unwrap_or("-"),
        result.summary.total_travel_time_s,
        result.summary.total_distance_m,
        selection
            .bucket
            .as_ref()
            .map_or("-".to_string(), |bucket| bucket.label.clone()),
        selection.engine_cache,
        selection.prepare_ms,
    );
    println!("written to {}", out.display());
    Ok(())
}
