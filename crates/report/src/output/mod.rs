use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_query::{
    MatrixResult, OdPairsDocument, OdResult, PointSetDocument, RouteBatchDocument,
    RouteBatchResult, RouteRequest, RouteResult, ServiceAreaRequest, ServiceAreaResult,
    ServiceAreaSequenceResult,
};
use serde::Serialize;

mod betweenness;
mod common;
mod gpkg;
mod matrix;
mod od;
mod route;
mod route_batch;
mod service_area;
mod service_area_sequence;

use betweenness::{write_betweenness_csv, write_betweenness_geojson, write_betweenness_gpkg};
use common::ensure_parent_dir;
use matrix::{
    write_matrix_csv, write_matrix_geojson, write_matrix_geoparquet, write_matrix_gpkg,
    write_matrix_parquet,
};
use od::{write_od_csv, write_od_geojson, write_od_geoparquet, write_od_gpkg, write_od_parquet};
use route::{
    write_route_csv, write_route_geojson, write_route_geoparquet, write_route_gpkg,
    write_route_parquet,
};
use route_batch::write_route_batch_gpkg;
use service_area::{
    write_service_area_csv, write_service_area_geojson, write_service_area_geoparquet,
    write_service_area_gpkg, write_service_area_parquet,
};
use service_area_sequence::write_service_area_sequence_geojson;

pub fn write_route_result(
    path: impl AsRef<Path>,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_route_csv(path, request, result),
        OutputFormat::GeoJson => write_route_geojson(path, request, result),
        OutputFormat::GeoPackage => write_route_gpkg(path, request, result),
        OutputFormat::Parquet => write_route_parquet(path, request, result),
        OutputFormat::GeoParquet => write_route_geoparquet(path, request, result),
    }
}

pub fn write_route_batch_result(
    path: impl AsRef<Path>,
    requests: &RouteBatchDocument,
    result: &RouteBatchResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::GeoPackage => write_route_batch_gpkg(path, requests, result),
        other => bail!(
            "route batch output does not support {:?}; use .json or .gpkg",
            other
        ),
    }
}

pub fn write_od_result(
    path: impl AsRef<Path>,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_od_csv(path, request, result),
        OutputFormat::GeoJson => write_od_geojson(path, request, result),
        OutputFormat::GeoPackage => write_od_gpkg(path, request, result),
        OutputFormat::Parquet => write_od_parquet(path, request, result),
        OutputFormat::GeoParquet => write_od_geoparquet(path, request, result),
    }
}

pub fn write_matrix_result(
    path: impl AsRef<Path>,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_matrix_csv(path, origins, destinations, result),
        OutputFormat::GeoJson => write_matrix_geojson(path, origins, destinations, result),
        OutputFormat::GeoPackage => write_matrix_gpkg(path, origins, destinations, result),
        OutputFormat::Parquet => write_matrix_parquet(path, origins, destinations, result),
        OutputFormat::GeoParquet => write_matrix_geoparquet(path, origins, destinations, result),
    }
}

pub fn write_service_area_result(
    path: impl AsRef<Path>,
    request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_service_area_csv(path, request, result),
        OutputFormat::GeoJson => write_service_area_geojson(path, request, result),
        OutputFormat::GeoPackage => write_service_area_gpkg(path, request, result),
        OutputFormat::Parquet => write_service_area_parquet(path, request, result),
        OutputFormat::GeoParquet => write_service_area_geoparquet(path, request, result),
    }
}

pub fn write_service_area_sequence_result(
    path: impl AsRef<Path>,
    result: &ServiceAreaSequenceResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::GeoJson => write_service_area_sequence_geojson(path, result),
        other => bail!(
            "service-area sequence output does not support {:?}; use .json or .geojson",
            other
        ),
    }
}

pub fn write_betweenness_result(
    path: impl AsRef<Path>,
    result: &netweevil_query::BetweennessResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_betweenness_csv(path, result),
        OutputFormat::GeoJson => write_betweenness_geojson(path, result),
        OutputFormat::GeoPackage => write_betweenness_gpkg(path, result),
        other => bail!(
            "betweenness output does not support {:?}; use .json, .csv, .geojson, or .gpkg",
            other
        ),
    }
}

#[derive(Debug, Clone, Copy)]
enum OutputFormat {
    Json,
    Csv,
    GeoJson,
    GeoPackage,
    Parquet,
    GeoParquet,
}

fn output_format(path: &Path) -> Result<OutputFormat> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
    {
        None => Ok(OutputFormat::Json),
        Some(ext) => match ext.as_str() {
            "json" => Ok(OutputFormat::Json),
            "csv" => Ok(OutputFormat::Csv),
            "geojson" => Ok(OutputFormat::GeoJson),
            "gpkg" | "geopackage" => Ok(OutputFormat::GeoPackage),
            "parquet" => Ok(OutputFormat::Parquet),
            "geoparquet" | "gpq" => Ok(OutputFormat::GeoParquet),
            other => {
                bail!(
                    "unsupported output extension '{other}'; use .json, .csv, .geojson, .gpkg, .parquet, or .geoparquet"
                )
            }
        },
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    serde_json::to_writer_pretty(BufWriter::new(file), value).context("serializing JSON")?;
    Ok(())
}

#[cfg(test)]
fn temp_path(name: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time works")
        .as_nanos();
    std::env::temp_dir().join(format!("netweevil-report-{unique}-{name}"))
}
