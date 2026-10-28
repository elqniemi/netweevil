use std::path::Path;

use anyhow::Result;
use netweevil_query::BetweennessResult;
use serde_json::json;

use super::common::{ensure_parent_dir, extent_for_features_z, write_csv};
use super::gpkg::{GeoPackageWriter, SqlValue};

pub(super) fn write_betweenness_csv(path: &Path, result: &BetweennessResult) -> Result<()> {
    write_csv(
        path,
        &[
            "edge_id",
            "source_way_id",
            "from_node_id",
            "to_node_id",
            "score",
            "normalized_score",
            "routed_pair_count",
            "geometry_wkt",
        ],
        result
            .edges
            .iter()
            .map(|edge| {
                vec![
                    edge.edge_id.to_string(),
                    edge.source_way_id.to_string(),
                    edge.from_node_id.to_string(),
                    edge.to_node_id.to_string(),
                    edge.score.to_string(),
                    edge.normalized_score.to_string(),
                    edge.routed_pair_count.to_string(),
                    super::common::linestring_wkt_z(&edge.geometry),
                ]
            })
            .collect(),
    )
}

pub(super) fn write_betweenness_geojson(path: &Path, result: &BetweennessResult) -> Result<()> {
    ensure_parent_dir(path)?;
    let document = json!({
        "type": "FeatureCollection",
        "properties": {
            "analysis_id": result.analysis_id,
            "routed_demand": result.routed_demand,
            "routed_pair_count": result.routed_pair_count,
            "unreachable_pair_count": result.unreachable_pair_count,
            "time_dependent": result.time_dependent,
        },
        "features": result.edges.iter().map(|edge| json!({
            "type": "Feature",
            "id": edge.edge_id,
            "properties": {
                "edge_id": edge.edge_id,
                "source_way_id": edge.source_way_id,
                "from_node_id": edge.from_node_id,
                "to_node_id": edge.to_node_id,
                "score": edge.score,
                "normalized_score": edge.normalized_score,
                "routed_pair_count": edge.routed_pair_count,
            },
            "geometry": {
                "type": "LineString",
                "coordinates": edge.geometry,
            }
        })).collect::<Vec<_>>()
    });
    std::fs::write(path, serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

pub(super) fn write_betweenness_gpkg(path: &Path, result: &BetweennessResult) -> Result<()> {
    let extent = extent_for_features_z(result.edges.iter().map(|edge| edge.geometry.as_slice()));
    let mut writer = GeoPackageWriter::create(path)?;
    writer.create_feature_table_3d(
        "edge_betweenness",
        &[
            ("edge_id", "INTEGER NOT NULL"),
            ("source_way_id", "INTEGER NOT NULL"),
            ("from_node_id", "INTEGER NOT NULL"),
            ("to_node_id", "INTEGER NOT NULL"),
            ("score", "REAL NOT NULL"),
            ("normalized_score", "REAL NOT NULL"),
            ("routed_pair_count", "INTEGER NOT NULL"),
        ],
        "LINESTRING",
        Some(extent),
    )?;
    for edge in &result.edges {
        writer.insert_feature_3d(
            "edge_betweenness",
            &[
                ("edge_id", SqlValue::Integer(i64::from(edge.edge_id))),
                ("source_way_id", SqlValue::Integer(edge.source_way_id)),
                (
                    "from_node_id",
                    SqlValue::Integer(i64::from(edge.from_node_id)),
                ),
                ("to_node_id", SqlValue::Integer(i64::from(edge.to_node_id))),
                ("score", SqlValue::Real(edge.score)),
                ("normalized_score", SqlValue::Real(edge.normalized_score)),
                (
                    "routed_pair_count",
                    SqlValue::Integer(edge.routed_pair_count as i64),
                ),
            ],
            &edge.geometry,
        )?;
    }
    Ok(())
}
