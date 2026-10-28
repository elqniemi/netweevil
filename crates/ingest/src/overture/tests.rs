use super::*;
use crate::test_util::TempDir;
use arrow_array::{ArrayRef, BinaryArray};
use arrow_schema::{Field, Fields, Schema};
use netweevil_core::AccessMask;
use serde_json::{Value, json};
use std::sync::Arc;

fn wkb_linestring(points: &[(f64, f64)]) -> Vec<u8> {
    let mut bytes = vec![1_u8];
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for (lon, lat) in points {
        bytes.extend_from_slice(&lon.to_le_bytes());
        bytes.extend_from_slice(&lat.to_le_bytes());
    }
    bytes
}

/// The subset of the Overture transportation segment schema the reader
/// consumes, including the pre-GA top-level `lanes` column.
fn attribute_schema() -> Schema {
    let when = DataType::Struct(Fields::from(vec![
        Field::new("heading", DataType::Utf8, true),
        Field::new("during", DataType::Utf8, true),
        Field::new(
            "mode",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
    ]));
    let speed = DataType::Struct(Fields::from(vec![
        Field::new("value", DataType::Float64, true),
        Field::new("unit", DataType::Utf8, true),
    ]));
    let between = DataType::List(Arc::new(Field::new("item", DataType::Float64, true)));
    let list_of = |fields: Vec<Field>| {
        DataType::List(Arc::new(Field::new(
            "item",
            DataType::Struct(Fields::from(fields)),
            true,
        )))
    };
    Schema::new(vec![
        Field::new("id", DataType::Utf8, true),
        Field::new("subtype", DataType::Utf8, true),
        Field::new("class", DataType::Utf8, true),
        Field::new("subclass", DataType::Utf8, true),
        Field::new(
            "names",
            DataType::Struct(Fields::from(vec![Field::new(
                "primary",
                DataType::Utf8,
                true,
            )])),
            true,
        ),
        Field::new(
            "connectors",
            list_of(vec![
                Field::new("connector_id", DataType::Utf8, true),
                Field::new("at", DataType::Float64, true),
            ]),
            true,
        ),
        Field::new(
            "access_restrictions",
            list_of(vec![
                Field::new("access_type", DataType::Utf8, true),
                Field::new("when", when.clone(), true),
                Field::new("between", between.clone(), true),
            ]),
            true,
        ),
        Field::new(
            "speed_limits",
            list_of(vec![
                Field::new("max_speed", speed, true),
                Field::new("when", when.clone(), true),
                Field::new("between", between.clone(), true),
            ]),
            true,
        ),
        Field::new(
            "road_surface",
            list_of(vec![
                Field::new("value", DataType::Utf8, true),
                Field::new("between", between.clone(), true),
            ]),
            true,
        ),
        Field::new(
            "road_flags",
            list_of(vec![
                Field::new(
                    "values",
                    DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
                    true,
                ),
                Field::new("between", between.clone(), true),
            ]),
            true,
        ),
        Field::new(
            "prohibited_transitions",
            list_of(vec![
                Field::new(
                    "sequence",
                    list_of(vec![
                        Field::new("connector_id", DataType::Utf8, true),
                        Field::new("segment_id", DataType::Utf8, true),
                    ]),
                    true,
                ),
                Field::new("final_heading", DataType::Utf8, true),
                Field::new("when", when, true),
            ]),
            true,
        ),
        Field::new(
            "lanes",
            list_of(vec![Field::new("direction", DataType::Utf8, true)]),
            true,
        ),
    ])
}

/// Writes segments (attribute JSON + WKB geometry) as a GeoParquet file.
fn write_fixture(path: &Path, rows: &[(Value, Vec<(f64, f64)>)]) {
    let attribute_schema = Arc::new(attribute_schema());
    let ndjson = rows
        .iter()
        .map(|(attributes, _)| attributes.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let attribute_batch = arrow_json::ReaderBuilder::new(Arc::clone(&attribute_schema))
        .build(std::io::Cursor::new(ndjson.as_bytes()))
        .expect("json reader builds")
        .next()
        .expect("one attribute batch")
        .expect("attribute batch decodes");

    let geometry: ArrayRef = Arc::new(BinaryArray::from_iter_values(
        rows.iter().map(|(_, line)| wkb_linestring(line)),
    ));
    let mut fields: Vec<Field> = attribute_schema
        .fields()
        .iter()
        .map(|field| field.as_ref().clone())
        .collect();
    fields.push(Field::new("geometry", DataType::Binary, true));
    let mut columns = attribute_batch.columns().to_vec();
    columns.push(geometry);
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).expect("batch assembles");

    let file = std::fs::File::create(path).expect("fixture file creates");
    let mut writer =
        parquet::arrow::ArrowWriter::try_new(file, schema, None).expect("writer builds");
    writer.write(&batch).expect("batch writes");
    writer.close().expect("writer closes");
}

fn scan_fixture(rows: &[(Value, Vec<(f64, f64)>)]) -> ScanOutput {
    let dir = TempDir::new("overture-scan");
    let path = dir.join("segments.parquet");
    write_fixture(&path, rows);
    scan_overture(&path, 0, &mut |_| {}).expect("fixture scans")
}

/// Residential road segment with connectors at the given linear positions.
fn road(id: &str, connectors: &[(&str, f64)]) -> Value {
    let connectors = connectors
        .iter()
        .map(|&(connector_id, at)| json!({"connector_id": connector_id, "at": at}))
        .collect::<Vec<_>>();
    json!({
        "id": id,
        "subtype": "road",
        "class": "residential",
        "connectors": connectors,
    })
}

#[test]
fn parses_wkb_linestrings_in_both_byte_orders() {
    let expected = Some(vec![(6.5, 53.2), (6.6, 53.3)]);
    let little = wkb_linestring(&[(6.5, 53.2), (6.6, 53.3)]);
    assert_eq!(parse_wkb_linestring(&little), expected);

    let mut big = vec![0_u8];
    big.extend_from_slice(&2_u32.to_be_bytes());
    big.extend_from_slice(&2_u32.to_be_bytes());
    for value in [6.5_f64, 53.2, 6.6, 53.3] {
        big.extend_from_slice(&value.to_be_bytes());
    }
    assert_eq!(parse_wkb_linestring(&big), expected);

    // A WKB point is not a usable segment geometry.
    let mut point = vec![1_u8];
    point.extend_from_slice(&1_u32.to_le_bytes());
    point.extend_from_slice(&6.5_f64.to_le_bytes());
    point.extend_from_slice(&53.2_f64.to_le_bytes());
    assert_eq!(parse_wkb_linestring(&point), None);
}

#[test]
fn joins_segments_through_shared_connectors() {
    let mut row_a = road("seg-a", &[("c1", 0.0), ("c2", 1.0)]);
    row_a["names"] = json!({"primary": "Main Street"});
    row_a["speed_limits"] = json!([{"max_speed": {"value": 50.0, "unit": "km/h"}}]);
    let row_b = road("seg-b", &[("c2", 0.0), ("c3", 1.0)]);

    let output = scan_fixture(&[
        (row_a, vec![(6.560, 53.20), (6.561, 53.20), (6.562, 53.20)]),
        (row_b, vec![(6.562, 53.20), (6.562, 53.21)]),
    ]);

    assert_eq!(output.pending_ways.len(), 2);
    let chunk_a = &output.pending_ways[0];
    let chunk_b = &output.pending_ways[1];
    assert_eq!(chunk_a.node_ids.len(), 3);
    assert_eq!(chunk_a.node_ids.last(), chunk_b.node_ids.first());
    assert_eq!(chunk_a.name.as_deref(), Some("Main Street"));
    assert!(chunk_a.forward_access_mask.contains(AccessMask::CAR));
    assert!(chunk_a.reverse_access_mask.contains(AccessMask::CAR));
    assert_eq!(chunk_a.forward_max_speed_kph, Some(50.0));
    assert_eq!(chunk_a.reverse_max_speed_kph, Some(50.0));
    assert_eq!(output.node_coords.len(), 4);
    let (lon, lat, z) = output.node_coords[chunk_a.node_ids.last().unwrap()];
    assert!((lon - 6.562).abs() < 1e-9 && (lat - 53.20).abs() < 1e-9);
    assert_eq!(z, 0.0);
}

#[test]
fn applies_heading_denies_and_scoped_rules_per_chunk() {
    let mut row = road("seg-a", &[("c1", 0.0), ("c2", 0.5), ("c3", 1.0)]);
    row["access_restrictions"] = json!([
        {"access_type": "denied", "when": {"heading": "backward"}},
    ]);
    row["speed_limits"] = json!([
        {"max_speed": {"value": 30.0, "unit": "km/h"}, "between": [0.0, 0.5]},
        {"max_speed": {"value": 50.0, "unit": "mph"}, "between": [0.5, 1.0]},
    ]);
    row["lanes"] = json!([
        {"direction": "forward"},
        {"direction": "forward"},
        {"direction": "backward"},
    ]);

    let output = scan_fixture(&[(row, vec![(6.560, 53.20), (6.561, 53.20), (6.562, 53.20)])]);

    assert_eq!(output.pending_ways.len(), 2);
    let first = &output.pending_ways[0];
    let second = &output.pending_ways[1];
    assert_eq!(first.reverse_access_mask.0, 0);
    assert_eq!(second.reverse_access_mask.0, 0);
    assert_eq!(first.forward_max_speed_kph, Some(30.0));
    assert!((second.forward_max_speed_kph.unwrap() - 80.4672).abs() < 1e-3);
    assert_eq!(first.forward_lanes, Some(2));
    assert_eq!(first.reverse_lanes, Some(1));
}

#[test]
fn skips_non_road_and_conditional_rules() {
    let mut rail = road("seg-rail", &[("r1", 0.0), ("r2", 1.0)]);
    rail["subtype"] = json!("rail");
    let mut conditional = road("seg-a", &[("c1", 0.0), ("c2", 1.0)]);
    conditional["access_restrictions"] = json!([
        {"access_type": "denied", "when": {"during": "Mo-Fr 07:00-09:00"}},
    ]);

    let output = scan_fixture(&[
        (rail, vec![(6.50, 53.20), (6.51, 53.20)]),
        (conditional, vec![(6.560, 53.20), (6.561, 53.20)]),
    ]);

    assert_eq!(output.pending_ways.len(), 1);
    // The time-scoped deny is not applied to the static edge.
    assert!(
        output.pending_ways[0]
            .forward_access_mask
            .contains(AccessMask::CAR)
    );
}

#[test]
fn imports_overture_fixture_end_to_end() {
    let dir = TempDir::new("overture-import");
    let fixture = dir.join("segments.parquet");
    let mut row = road("seg-a", &[("c1", 0.0), ("c2", 1.0)]);
    row["speed_limits"] = json!([{"max_speed": {"value": 60.0, "unit": "km/h"}}]);
    write_fixture(&fixture, &[(row, vec![(6.560, 53.20), (6.562, 53.20)])]);

    let paths =
        netweevil_persist::WorkspacePaths::discover(dir.path()).expect("workspace discovers");
    let manifest = crate::import_dataset(
        &paths,
        std::slice::from_ref(&fixture),
        crate::DatasetImportOptions {
            name: "overture-test".to_string(),
            source: fixture.display().to_string(),
            ..Default::default()
        },
        |_| {},
    )
    .expect("overture dataset imports");

    assert_eq!(
        manifest.source_format,
        netweevil_core::SourceFormat::OvertureParquet
    );
    let meta = manifest.topology_meta.expect("topology meta present");
    assert_eq!(meta.node_count, 2);
    assert_eq!(meta.edge_count, 2);

    let bundle = netweevil_persist::read_topology_bundle(
        &manifest.topology_bundle.expect("bundle ref").path,
    )
    .expect("bundle reads back");
    assert_eq!(
        bundle.schema_version,
        netweevil_core::TOPOLOGY_BUNDLE_SCHEMA_VERSION
    );
    assert_eq!(bundle.edge_profile(0).max_speed_kph, Some(60.0));
    assert_eq!(bundle.regions.timezones, ["Europe/Amsterdam"]);
    assert_eq!(bundle.regions.node_timezone, vec![0, 0]);
    assert!(!bundle.regions.has_admin_areas());
    assert!(manifest.elevation_bundle.is_none());
}

#[test]
fn imports_with_a_dem_and_admin_boundaries() {
    let dir = TempDir::new("overture-enriched");
    let fixture = dir.join("segments.parquet");
    let mut bridge = road("seg-b", &[("c2", 0.0), ("c3", 1.0)]);
    bridge["road_flags"] = json!([{"values": ["is_bridge"]}]);
    write_fixture(
        &fixture,
        &[
            (
                road("seg-a", &[("c1", 0.0), ("c2", 1.0)]),
                vec![(6.560, 53.20), (6.562, 53.20)],
            ),
            (bridge, vec![(6.562, 53.20), (6.564, 53.20), (6.566, 53.20)]),
            (
                road("seg-c", &[("c3", 0.0), ("c4", 1.0)]),
                vec![(6.566, 53.20), (6.568, 53.20)],
            ),
        ],
    );
    // 0.001 degree cells from 6.55 E; each holds its column index, except a
    // 0 m river in column 14 under the middle of the bridge.
    let dem = dir.join("dem.asc");
    let mut grid =
        "ncols 30\nnrows 20\nxllcorner 6.55\nyllcorner 53.19\ncellsize 0.001\n".to_string();
    for _ in 0..20 {
        for column in 0..30 {
            grid.push_str(&format!(" {}", if column == 14 { 0 } else { column }));
        }
        grid.push('\n');
    }
    std::fs::write(&dem, grid).unwrap();
    // A state west of 6.565 E without its country boundary, and a country east of it.
    let boundaries = dir.join("boundaries.geojson");
    std::fs::write(
        &boundaries,
        r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"ISO3166-2":"NL-GR","name":"Groningen"},
           "geometry":{"type":"Polygon","coordinates":[[[6.5,53.1],[6.565,53.1],[6.565,53.3],[6.5,53.3],[6.5,53.1]]]}},
          {"type":"Feature","properties":{"admin_level":"2","ISO3166-1":"DE","name":"Deutschland"},
           "geometry":{"type":"Polygon","coordinates":[[[6.565,53.1],[6.6,53.1],[6.6,53.3],[6.565,53.3],[6.565,53.1]]]}}
        ]}"#,
    )
    .unwrap();

    let paths =
        netweevil_persist::WorkspacePaths::discover(dir.path()).expect("workspace discovers");
    let options = |name: &str| crate::DatasetImportOptions {
        name: name.to_string(),
        source: fixture.display().to_string(),
        ..Default::default()
    };
    let plain = crate::import_dataset(&paths, std::slice::from_ref(&fixture), options("t"), |_| {})
        .expect("plain import");
    let manifest = crate::import_dataset(
        &paths,
        std::slice::from_ref(&fixture),
        crate::DatasetImportOptions {
            admin_boundaries: Some(boundaries),
            dem: Some(crate::DemImportOptions {
                path: dem,
                crs: None,
                vertical_datum: Some("NAP".to_string()),
            }),
            ..options("t")
        },
        |_| {},
    )
    .expect("enriched import");
    assert_ne!(
        plain.topology_bundle.unwrap().bundle_id,
        manifest.topology_bundle.as_ref().unwrap().bundle_id,
        "enrichment inputs change the bundle id"
    );

    let bundle = netweevil_persist::read_topology_bundle(&manifest.topology_bundle.unwrap().path)
        .expect("bundle reads back");
    let node_at = |lon: f64| {
        bundle
            .nodes
            .iter()
            .position(|node| (node.lon - lon).abs() < 1e-9)
            .unwrap()
    };
    let z = |lon: f64| bundle.nodes[node_at(lon)].z;
    assert_eq!((z(6.562), z(6.566)), (11.5, 15.5));
    // The bridge's middle vertex spans the river instead of dipping into it.
    assert_eq!(z(6.564), 13.5);
    let summary = bundle.elevation.dem.as_ref().expect("DEM summary");
    assert_eq!(summary.structure_node_count, 1);
    assert_eq!(summary.source.vertical_datum.as_deref(), Some("NAP"));
    assert_eq!(
        bundle.elevation.edge_max_uphill_permille.len(),
        bundle.edge_count()
    );
    let uphill_edge = (0..bundle.edge_count())
        .find(|&edge| {
            let routing = bundle.routing_edge(edge);
            routing.from.0 as usize == node_at(6.560) && routing.to.0 as usize == node_at(6.562)
        })
        .unwrap();
    assert_eq!(bundle.routing_edge(uphill_edge).ascent_m, 2.0);
    assert!(bundle.elevation.edge_max_grades(uphill_edge).unwrap().0 > 0.0);

    let west = bundle.regions.node_admin_area(node_at(6.564)).unwrap();
    assert_eq!(
        (west.country_code.as_str(), west.state_code.as_deref()),
        ("NL", Some("NL-GR"))
    );
    let east = bundle.regions.node_admin_area(node_at(6.566)).unwrap();
    assert_eq!(east.country_name.as_deref(), Some("Deutschland"));
    let crossing = (0..bundle.edge_count())
        .filter(|&edge| bundle.edge_crosses_border(edge))
        .count();
    assert_eq!(crossing, 2, "both directions of the bridge's eastern half");

    let raster = manifest.elevation_bundle.expect("elevation raster written");
    let sampler = crate::ElevationSampler::open(std::path::Path::new(&raster.path)).unwrap();
    let height = sampler.sample(6.5675, 53.2005).unwrap().unwrap();
    assert!((height - 17.0).abs() < 1e-6, "{height}");
}

#[test]
fn maps_prohibited_transitions_to_restriction_candidates() {
    let mut row_a = road("seg-a", &[("c1", 0.0), ("c2", 1.0)]);
    row_a["prohibited_transitions"] = json!([
        {
            "sequence": [{"connector_id": "c2", "segment_id": "seg-b"}],
            "final_heading": "forward",
        },
    ]);
    let row_b = road("seg-b", &[("c2", 0.0), ("c3", 1.0)]);

    let output = scan_fixture(&[
        (row_a, vec![(6.560, 53.20), (6.562, 53.20)]),
        (row_b, vec![(6.562, 53.20), (6.562, 53.21)]),
    ]);

    assert_eq!(output.restriction_candidates.len(), 1);
    let candidate = &output.restriction_candidates[0];
    assert_eq!(candidate.from_way_id, output.pending_ways[0].osm_way_id);
    assert_eq!(candidate.to_way_id, output.pending_ways[1].osm_way_id);
    let shared_node = *output.pending_ways[0].node_ids.last().unwrap();
    assert!(matches!(candidate.via, ViaSpec::Node(node) if node == shared_node));

    // The candidate resolves through the shared topology build.
    let (bundle, _, _) = crate::topology::build_topology_from_scan(
        Path::new("fixture.parquet"),
        output,
        &mut |_| {},
    )
    .expect("topology builds");
    assert_eq!(bundle.turn_restrictions.len(), 1);
    assert_eq!(bundle.turn_restrictions[0].edge_path.len(), 2);
}
