//! `netweevil indoor`: import, list, show and remove indoor sites, and
//! `netweevil analyze indoor-route`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use netweevil_core::LocalFrame;
use netweevil_ingest::indoor::{IndoorSourceOptions, read_indoor_source};
use netweevil_persist::indoor::{
    read_indoor_site_manifest, read_indoor_site_manifests, remove_indoor_site,
};
use netweevil_persist::{WorkspacePaths, write_json};
use netweevil_profile::load_profile;
use netweevil_query::PreparedRoutingEngine;
use netweevil_query::read_request_document;
use netweevil_site::geojson::indoor_route_geojson;
use netweevil_site::{
    DoorToDoorRequest, IndoorRouteRequest, SiteCatalog, build_site, door_to_door_geojson,
    door_to_door_route, route_indoor,
};
use std::sync::Arc;

use crate::analyze::load_route_execution_inputs;

#[derive(Subcommand, Debug)]
pub(crate) enum IndoorCommand {
    /// Import an indoor site from an engine input document or OSM data.
    ///
    /// The source is an engine input JSON in local metres (needs --anchor),
    /// in WGS84 (`"crs": "EPSG:4326"`) or in a projected CRS (--crs), or an
    /// `.osm.pbf`/`.osm` file with Simple Indoor Tagging.
    Import(IndoorImportArgs),
    /// List the imported indoor sites.
    List,
    /// Print a site's manifest: anchor, levels, entrances and counts.
    Show {
        /// Site id.
        site_id: String,
    },
    /// Remove a site and its bundle.
    Remove {
        /// Site id.
        site_id: String,
    },
}

#[derive(Args, Debug)]
pub(crate) struct IndoorImportArgs {
    /// Engine input JSON, `.osm.pbf` or `.osm` file.
    source: PathBuf,
    /// Site id (letters, digits, `-`, `_`, `.`).
    #[arg(long)]
    name: String,
    /// Display name; defaults to the id.
    #[arg(long)]
    label: Option<String>,
    /// Site origin `lon,lat` for local metre coordinates. For other sources
    /// it replaces the centroid as the origin.
    #[arg(long, value_parser = parse_lon_lat)]
    anchor: Option<[f64; 2]>,
    /// Angle from east to the site's x axis, counter-clockwise, degrees.
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    rotation: f64,
    /// Height of the site's z = 0 in metres, added to output z.
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    elevation: f64,
    /// Projected CRS of the input coordinates (`EPSG:<code>` or `+proj=...`).
    #[arg(long)]
    crs: Option<String>,
    /// OSM only: `min_lon,min_lat,max_lon,max_lat` filter.
    #[arg(long, value_parser = parse_bbox, allow_hyphen_values = true)]
    bbox: Option<[f64; 4]>,
}

#[derive(Args, Debug)]
pub(crate) struct IndoorRouteArgs {
    /// Indoor route request (JSON or YAML).
    #[arg(long)]
    request: PathBuf,
    /// Result file: `.json` writes the result, `.geojson` one feature per
    /// segment. Omit it to write JSON to the runs directory.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct DoorToDoorArgs {
    /// Id of an imported street dataset.
    #[arg(long)]
    dataset: String,
    /// Street routing profile file (YAML or TOML), compiled for the dataset.
    #[arg(long)]
    profile: PathBuf,
    /// Door-to-door request (JSON or YAML).
    #[arg(long)]
    request: PathBuf,
    /// Result file: `.json` writes the result, `.geojson` one feature per
    /// leg. Omit it to write JSON to the runs directory.
    #[arg(long)]
    out: Option<PathBuf>,
}

fn parse_numbers<const N: usize>(s: &str) -> Result<[f64; N], String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("'{s}': {e}"))?;
    v.try_into()
        .map_err(|_| format!("'{s}': expected {N} comma-separated numbers"))
}

fn parse_lon_lat(s: &str) -> Result<[f64; 2], String> {
    let [lon, lat] = parse_numbers::<2>(s)?;
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err(format!("'{s}' is not lon,lat in degrees"));
    }
    Ok([lon, lat])
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let b = parse_numbers::<4>(s)?;
    if b[0] >= b[2] || b[1] >= b[3] {
        return Err(format!("'{s}' is not min_lon,min_lat,max_lon,max_lat"));
    }
    Ok(b)
}

pub(crate) fn indoor_command(paths: &WorkspacePaths, command: IndoorCommand) -> Result<()> {
    match command {
        IndoorCommand::Import(args) => indoor_import(paths, args),
        IndoorCommand::List => indoor_list(paths),
        IndoorCommand::Show { site_id } => {
            let manifest = read_indoor_site_manifest(paths, &site_id)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
            Ok(())
        }
        IndoorCommand::Remove { site_id } => {
            remove_indoor_site(paths, &site_id)?;
            println!("removed indoor site '{site_id}'");
            Ok(())
        }
    }
}

fn indoor_import(paths: &WorkspacePaths, args: IndoorImportArgs) -> Result<()> {
    let anchor = args.anchor.map(|[lon, lat]| LocalFrame {
        lon,
        lat,
        rotation_deg: args.rotation,
        elevation_m: args.elevation,
    });
    if anchor.is_none() && (args.rotation != 0.0 || args.elevation != 0.0) {
        bail!("--rotation and --elevation need --anchor");
    }
    let options = IndoorSourceOptions {
        anchor,
        crs: args.crs.clone(),
        bbox: args.bbox,
    };
    let source = read_indoor_source(&args.source, &options)?;
    if let Some(stats) = &source.osm {
        println!(
            "osm: {} rooms, {} corridors, {} areas, {} doors, {} entrances, {} edges, {} stairs, {} lifts, {} escalators, {} travelators, {} barriers, {} pois on {} levels (storey height {:.2} m)",
            stats.rooms,
            stats.corridors,
            stats.areas,
            stats.doors,
            stats.entrances,
            stats.edges,
            stats.stairs,
            stats.lifts,
            stats.escalators,
            stats.travelators,
            stats.barriers,
            stats.pois,
            stats.levels,
            stats.storey_height_m,
        );
        if stats.skipped_doors > 0 {
            println!(
                "osm: {} doors or entrances skipped (not on a room, corridor or area outline)",
                stats.skipped_doors
            );
        }
    }
    let built = build_site(
        paths,
        &args.name,
        args.label,
        source.input,
        source.frame,
        source.source,
    )?;
    let m = &built.manifest;
    let c = &m.counts;
    println!(
        "indoor site '{}': {} levels, {} surfaces, {} edges, {} portals, {} verticals, {} pois, {} entrances",
        m.site_id, c.levels, c.surfaces, c.edges, c.portals, c.verticals, c.pois, c.entrances
    );
    println!(
        "graph: {} nodes, {} arcs, {} triangles, compiled in {:.0} ms",
        c.graph_nodes, c.graph_arcs, c.triangles, built.report.millis
    );
    println!(
        "anchor: {:.7}, {:.7} (rotation {} deg)",
        m.anchor.lon, m.anchor.lat, m.anchor.rotation_deg
    );
    for e in &m.entrances {
        println!(
            "entrance {} ({:?}, level {}): outdoor side {:.7}, {:.7}{}",
            e.id,
            e.kind,
            e.level.as_deref().unwrap_or("-"),
            e.outdoor_lon,
            e.outdoor_lat,
            if e.step_free { "" } else { ", not step-free" }
        );
    }
    if m.entrances.is_empty() {
        println!(
            "warning: the site has no entrance; tag a portal, node or POI `entrance` to join it to streets"
        );
    }
    for w in &m.warnings {
        println!("warning: {w}");
    }
    Ok(())
}

fn indoor_list(paths: &WorkspacePaths) -> Result<()> {
    let manifests = read_indoor_site_manifests(paths)?;
    if manifests.is_empty() {
        println!("no indoor sites; import one with `netweevil indoor import`");
    }
    for m in manifests {
        println!(
            "{}\t{}\t{} levels\t{} entrances\t{:.6},{:.6}\t{}",
            m.site_id,
            m.label,
            m.levels.len(),
            m.entrances.len(),
            m.anchor.lon,
            m.anchor.lat,
            m.source.path
        );
    }
    Ok(())
}

fn wants_geojson(path: Option<&Path>) -> bool {
    path.and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("geojson"))
}

pub(crate) fn analyze_door_to_door(paths: &WorkspacePaths, args: DoorToDoorArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let request: DoorToDoorRequest = read_request_document(&args.request, "door-to-door request")?;
    let (topology, acceleration, _, compiled) =
        load_route_execution_inputs(paths, &args.dataset, &profile)?;
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let catalog = SiteCatalog::new(paths.clone());
    let result = door_to_door_route(&engine, &catalog, &request)
        .with_context(|| format!("door-to-door route '{}'", request.route_id))?;
    let out = args.out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("door-to-door-{}.json", request.route_id))
    });
    if wants_geojson(Some(&out)) {
        write_json(&out, &door_to_door_geojson(&result))?;
    } else {
        write_json(&out, &result)?;
    }
    for w in &result.warnings {
        eprintln!("warning: {w}");
    }
    let legs: Vec<String> = result
        .legs
        .iter()
        .map(|l| match &l.site_id {
            Some(site) => format!("indoor[{site}] {:.0} s", l.duration_s),
            None => format!("street {:.0} s", l.duration_s),
        })
        .collect();
    println!(
        "door-to-door '{}': {:.0} s, {:.0} m: {}",
        result.route_id,
        result.summary.duration_s,
        result.summary.length_m,
        legs.join(" > ")
    );
    for (side, e) in [
        ("origin", &result.origin_entrance),
        ("destination", &result.destination_entrance),
    ] {
        if let Some(e) = e {
            println!(
                "{side} entrance: {} of '{}' (indoor {:.0} s, snapped {:.1} m from the street)",
                e.entrance.id, e.site_id, e.indoor_duration_s, e.snap_distance_m
            );
        }
    }
    println!("door-to-door result written to {}", out.display());
    Ok(())
}

pub(crate) fn analyze_indoor_route(paths: &WorkspacePaths, args: IndoorRouteArgs) -> Result<()> {
    let request: IndoorRouteRequest = read_request_document(&args.request, "indoor route request")?;
    let catalog = SiteCatalog::new(paths.clone());
    let site = catalog.get(&request.site_id)?;
    let result = route_indoor(&site, &request)
        .with_context(|| format!("routing in indoor site '{}'", request.site_id))?;
    let out = args.out.unwrap_or_else(|| {
        paths.runs_dir.join(format!(
            "indoor-route-{}.json",
            request.route_id.as_deref().unwrap_or(&request.site_id)
        ))
    });
    if wants_geojson(Some(&out)) {
        write_json(&out, &indoor_route_geojson(&result))?;
    } else {
        write_json(&out, &result)?;
    }
    for w in &result.warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "indoor route in '{}': {:.0} s, {:.1} m, climb {:.1} m, {} segments, spaces {}",
        result.site_id,
        result.summary.duration_s,
        result.summary.length_m,
        result.summary.climb_m,
        result.segments.len(),
        result.spaces.join(" > ")
    );
    println!("indoor route result written to {}", out.display());
    Ok(())
}
