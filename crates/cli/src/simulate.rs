use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use netweevil_core::CompiledProfileBundle;
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_topology_bundle,
    write_run_manifest,
};
use netweevil_profile::{ProfileDocument, load_profile};
use netweevil_query::PreparedRoutingEngine;
use netweevil_report::{RunKind, RunStatus, new_run_manifest};
use netweevil_simulate::{
    SimRunState, SimulationRunner, edge_bins_to_geojson, edge_usage_to_geojson,
    frames_to_temporal_geojson, load_scenario,
};

use crate::software_info;

#[derive(Subcommand, Debug)]
pub(crate) enum SimulateCommand {
    /// Validate a simulation scenario file without running it.
    Validate { scenario: PathBuf },
    /// Run an agent-based traffic simulation scenario.
    Run(SimulateRunArgs),
}

#[derive(Args, Debug)]
pub(crate) struct SimulateRunArgs {
    /// Scenario file (YAML/TOML/JSON).
    scenario: PathBuf,
    #[arg(long)]
    dataset: String,
    /// Profile files; every fleet's profile_id must resolve to one of these
    /// (compile them first with `netweevil profile compile`).
    #[arg(long = "profile")]
    profiles: Vec<PathBuf>,
    /// Output directory (defaults to .netweevil/runs).
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// Also write a temporal GeoJSON of agent frames for the QGIS
    /// temporal controller.
    #[arg(long)]
    temporal_geojson: bool,
    /// Minimum traversals for an edge to appear in the busy-segments output.
    #[arg(long, default_value_t = 1)]
    min_traversals: u32,
}

pub(crate) fn simulate_validate(scenario_path: &Path) -> Result<()> {
    let scenario = load_scenario(scenario_path)?;
    println!(
        "scenario '{}' is valid: {} fleet(s), {} zone(s), {:.0}s horizon at {:.2}s ticks",
        scenario.scenario.id,
        scenario.fleets.len(),
        scenario.zones.len(),
        scenario.time.duration_s,
        scenario.time.tick_s,
    );
    for fleet in &scenario.fleets {
        println!(
            "  fleet '{}': {} agent(s) via profile '{}'",
            fleet.fleet_id, fleet.agent_count, fleet.profile_id
        );
    }
    Ok(())
}

pub(crate) fn simulate_run(paths: &WorkspacePaths, args: SimulateRunArgs) -> Result<()> {
    let scenario = load_scenario(&args.scenario)?;
    if args.profiles.is_empty() {
        anyhow::bail!(
            "pass at least one --profile covering the fleet profile_ids: {}",
            scenario
                .fleets
                .iter()
                .map(|fleet| fleet.profile_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let dataset_manifest = read_dataset_manifest(paths, &args.dataset)
        .with_context(|| format!("reading dataset manifest for '{}'", args.dataset))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;
    println!("loading topology bundle {}", topology_ref.path);
    let topology = Arc::new(
        read_topology_bundle(&topology_ref.path)
            .with_context(|| format!("reading topology bundle {}", topology_ref.path))?,
    );
    let acceleration = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
                .map(Arc::new)
        })
        .transpose()?;
    if acceleration.is_none() {
        eprintln!(
            "warning: dataset '{}' has no acceleration bundle; agent dispatch will fall back to \
             plain bidirectional Dijkstra and can be orders of magnitude slower. Re-import the \
             dataset with `netweevil dataset import` to build one.",
            args.dataset
        );
    }

    let compiled_manifests = read_compiled_profile_manifests(paths)?;
    let mut engines: BTreeMap<String, Arc<PreparedRoutingEngine>> = BTreeMap::new();
    let mut first_profile_document: Option<ProfileDocument> = None;
    for profile_path in &args.profiles {
        let document = load_profile(profile_path)
            .with_context(|| format!("loading {}", profile_path.display()))?;
        document.validate()?;
        let wanted_hash = document.fingerprint()?;
        let compiled_manifest = compiled_manifests
            .iter()
            .find(|manifest| {
                manifest.dataset_id.0 == args.dataset && manifest.profile_hash == wanted_hash
            })
            .with_context(|| {
                format!(
                    "compiled bundle for profile '{}' not found; run `netweevil profile compile --dataset {} --profile {}` first",
                    document.profile.id,
                    args.dataset,
                    profile_path.display()
                )
            })?;
        let compiled_bundle: CompiledProfileBundle =
            read_compiled_profile_bundle(&compiled_manifest.bundle.path).with_context(|| {
                format!(
                    "reading compiled profile bundle {}",
                    compiled_manifest.bundle.path
                )
            })?;
        println!(
            "profile '{}' ready ({} mode)",
            document.profile.id,
            serde_json::to_string(&document.profile.mode)?.trim_matches('"')
        );
        let engine = Arc::new(
            PreparedRoutingEngine::new(
                topology.clone(),
                Arc::new(compiled_bundle),
                acceleration.clone(),
            )
            .with_context(|| format!("preparing routing engine for '{}'", document.profile.id))?,
        );
        if first_profile_document.is_none() {
            first_profile_document = Some(document.clone());
        }
        engines.insert(document.profile.id.clone(), engine);
    }
    let first_profile_document =
        first_profile_document.context("no profile documents were loaded")?;

    let runner = SimulationRunner::new(scenario.clone(), topology, engines)
        .context("building simulation runner")?;
    let network = runner.network();
    let handle = runner.handle();

    // Progress reporter while the engine runs on this thread.
    let progress_handle = handle.clone();
    let reporter = std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let status = progress_handle.status();
            match status.state {
                SimRunState::Completed | SimRunState::Cancelled | SimRunState::Failed => break,
                SimRunState::Pending => {}
                _ => {
                    println!(
                        "  t={:>6.0}s active={} arrived={}/{} frames={}",
                        status.sim_time_s,
                        status.agents_active,
                        status.agents_arrived,
                        status.agents_total,
                        status.frames_available,
                    );
                }
            }
        }
    });

    println!(
        "running scenario '{}' (seed {})",
        scenario.scenario.id, scenario.scenario.seed
    );
    let result = runner.run().context("running simulation")?;
    let _ = reporter.join();

    let out_dir = args
        .out_dir
        .clone()
        .unwrap_or_else(|| paths.runs_dir.clone());
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output directory {}", out_dir.display()))?;
    let stem = format!("simulation-{}", scenario.scenario.id);

    let result_path = out_dir.join(format!("{stem}-result.json"));
    netweevil_persist::write_compact_json(&result_path, &result)?;

    let busy_path = out_dir.join(format!("{stem}-busy-segments.geojson"));
    netweevil_persist::write_compact_json(
        &busy_path,
        &edge_usage_to_geojson(&result.edge_usage, &network, args.min_traversals),
    )?;

    let congestion_path = out_dir.join(format!("{stem}-congestion.geojson"));
    netweevil_persist::write_compact_json(
        &congestion_path,
        &edge_bins_to_geojson(&result.edge_bins, &network, 0.0),
    )?;

    let mut temporal_path = None;
    if args.temporal_geojson {
        let fleet_ids: Vec<String> = result
            .fleets
            .iter()
            .map(|fleet| fleet.fleet_id.clone())
            .collect();
        let path = out_dir.join(format!("{stem}-temporal.geojson"));
        netweevil_persist::write_compact_json(
            &path,
            &frames_to_temporal_geojson(
                &result.frames,
                &network,
                &fleet_ids,
                "2026-01-01T00:00:00",
            ),
        )?;
        temporal_path = Some(path);
    }

    let mut manifest = new_run_manifest(
        RunKind::Simulation,
        args.dataset.clone(),
        &first_profile_document,
        args.scenario.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Simulation '{}' moved {} agent(s) across {} fleet(s): {} arrived, {} reroute(s), {:.0}s simulated in {} ms wall time.",
            scenario.scenario.id,
            result.summary.agents_total,
            result.fleets.len(),
            result.summary.agents_arrived,
            result.summary.total_reroutes,
            result.summary.simulated_s,
            result.summary.wall_time_ms,
        ),
        software_info(),
        None,
    )?;
    manifest.algorithm.engine = "mesoscopic_queue_simulation".to_string();
    manifest.algorithm.graph_model = "directed_edge_graph".to_string();
    manifest.methods_summary.plain_language =
        "Agent-based mesoscopic queue simulation: exact shortest-path dispatch per agent, \
         density-dependent edge speeds, storage/outflow capacities with spillback, traffic \
         signal gates, and live congested rerouting. Deterministic for a fixed scenario seed."
            .to_string();
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;

    println!("simulation result written to {}", result_path.display());
    println!("busy segments written to {}", busy_path.display());
    println!("congestion bins written to {}", congestion_path.display());
    if let Some(path) = temporal_path {
        println!("temporal frames written to {}", path.display());
    }
    println!("run manifest written to {}", manifest_path.display());
    for fleet in &result.fleets {
        println!(
            "  fleet '{}' [{}]: dispatched {}/{} arrived {} mean travel {:.0}s mean delay {:.0}s reroutes {}",
            fleet.fleet_id,
            fleet.mode,
            fleet.dispatched,
            fleet.requested,
            fleet.arrived,
            fleet.mean_travel_time_s.unwrap_or(0.0),
            fleet.mean_delay_s.unwrap_or(0.0),
            fleet.reroutes,
        );
    }
    Ok(())
}
