use super::*;

fn topology() -> TopologyBundle {
    let mut bundle = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "city.osm.pbf".to_string(),
        source_sha256: "abc".to_string(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 1_141_000_000_f64 * 1e-7,
                lat: 223_000_000_f64 * 1e-7,
                z: f64::NAN,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 114.2,
                lat: 22.4,
                z: f64::NAN,
            },
        ],
        edge_layers: TopologyEdgeLayers {
            routing: vec![
                RoutingEdge {
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 42,
                    length_m: 100,
                    ..RoutingEdge::default()
                },
                RoutingEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(0),
                    source_way_id: 42,
                    length_m: 100,
                    temporal_rule_id: Some(3),
                    source_direction: -1,
                    ..RoutingEdge::default()
                },
            ],
            profile: vec![EdgeProfileAttributes::default(); 2],
            presentation: vec![
                EdgePresentation {
                    name_index: Some(0),
                    ..EdgePresentation::default()
                },
                EdgePresentation::default(),
            ],
            vehicle_restrictions: VehicleRestrictionTable::default(),
        },
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: EdgeBasedTopology::default(),
        spatial_index: None,
        node_component_ids: vec![0, 0],
        edge_component_ids: vec![0, 0],
        feature_attributes: FeatureAttributeTable::default(),
        temporal_rule_sets: Vec::new(),
        regions: Default::default(),
        elevation: Default::default(),
    };
    bundle.edge_based_topology = EdgeBasedTopology::build(2, 2, |index| {
        let edge = bundle.edge_layers.routing[index];
        (edge.from.0, edge.to.0)
    });
    bundle.spatial_index = NodeSpatialIndex::build(&bundle.nodes);
    bundle
}

fn topology_bytes(bundle: &TopologyBundle) -> Vec<u8> {
    let mut writer = SectionWriter::new();
    encode_topology(&mut writer, bundle).unwrap();
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    bytes
}

#[test]
fn derived_and_explicit_structures_round_trip() {
    let bundle = topology();
    let read = read_topology_sectioned(&topology_bytes(&bundle), Path::new("t.bin"))
        .expect("current topology reads");
    assert_eq!(read.edge_based_topology, bundle.edge_based_topology);
    assert_eq!(read.spatial_index, bundle.spatial_index);
    assert_eq!(read.nodes[0].lon.to_bits(), bundle.nodes[0].lon.to_bits());
    assert_eq!(read.nodes[1].lat.to_bits(), bundle.nodes[1].lat.to_bits());
    assert!(read.nodes[1].z.is_nan());
    assert_eq!(read.routing_edge(1).temporal_rule_id, Some(3));
    assert_eq!(read.routing_edge(1).source_direction, -1);
    assert_eq!(read.edge_presentation(0).name_index, Some(0));

    // Structures that do not match their rebuild are stored verbatim.
    let mut edited = bundle.clone();
    edited.edge_based_topology.edge_transition_edges.reverse();
    edited.edge_component_ids = vec![5, 6];
    if let Some(index) = edited.spatial_index.as_mut() {
        index.columns += 1;
    }
    let read = read_topology_sectioned(&topology_bytes(&edited), Path::new("t.bin"))
        .expect("explicit structures read");
    assert_eq!(read.edge_based_topology, edited.edge_based_topology);
    assert_eq!(read.edge_component_ids, vec![5, 6]);
    assert_eq!(read.spatial_index, edited.spatial_index);
}

#[test]
fn large_sections_deflate_and_round_trip() {
    // Spans three deflate chunks.
    let compressible: Vec<u8> = (0..2_500_000_u32).map(|value| (value % 7) as u8).collect();
    let small = vec![1_u8, 2, 3];
    let mut writer = SectionWriter::new();
    writer.write_section(compressible.clone()).unwrap();
    writer.write_section(small.clone()).unwrap();
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert!(
        bytes.len() < compressible.len() / 4,
        "large section is deflated"
    );
    let mut reader = SectionReader::new(&bytes, Path::new("b.bin")).unwrap();
    assert_eq!(&*reader.next_section().unwrap(), compressible.as_slice());
    assert_eq!(&*reader.next_section().unwrap(), small.as_slice());
    assert!(reader.next_section().is_err());

    let header = MAGIC_LEN;
    bytes.truncate(bytes.len() - 4);
    assert!(SectionReader::new(&bytes, Path::new("b.bin")).is_err());
    bytes[header + 9] ^= 0xff;
    assert!(SectionReader::new(&bytes[..header + 40], Path::new("b.bin")).is_err());
}

#[test]
fn rejects_an_unrecognized_magic_or_schema() {
    let mut bytes = topology_bytes(&topology());
    bytes[..8].copy_from_slice(b"INVALID!");
    let error = read_topology_sectioned(&bytes, Path::new("topology.bin"))
        .expect_err("an unrecognized magic is rejected");
    assert!(
        error.to_string().contains("re-import the dataset"),
        "{error}"
    );

    let mut stale = topology();
    stale.schema_version -= 1;
    let error = read_topology_sectioned(&topology_bytes(&stale), Path::new("topology.bin"))
        .expect_err("a mismatched schema version is rejected");
    assert!(
        error.to_string().contains("re-import the dataset"),
        "{error}"
    );
}

#[test]
fn elevation_raster_tiles_load_individually() {
    use netweevil_core::{DemFormat, DemSourceInfo, RasterCrs, RasterGrid};
    let grid = RasterGrid {
        columns: 5,
        rows: 3,
        min_x: 0.0,
        max_y: 30.0,
        cell_width: 10.0,
        cell_height: 10.0,
    };
    let header = ElevationRasterHeader {
        schema_version: ELEVATION_RASTER_SCHEMA_VERSION,
        source: DemSourceInfo {
            path: "dem.asc".to_string(),
            sha256: "abc".to_string(),
            format: DemFormat::AsciiGrid,
            crs: RasterCrs::Geographic,
            vertical_datum: None,
            grid,
        },
        grid,
        tile_size: 2,
    };
    assert_eq!((header.tile_columns(), header.tile_rows()), (3, 2));
    assert_eq!(header.tile_dimensions(2), (1, 2));
    assert_eq!(header.tile_dimensions(5), (1, 1));
    assert_eq!(header.cell_tile(4, 2), (5, 0));
    assert_eq!(header.cell_tile(3, 1), (1, 3));
    // Cell (column, row) holds `row * 10 + column + 0.5`, except one nodata cell.
    let tiles = (0..header.tile_count())
        .map(|tile| {
            let (columns, rows) = header.tile_dimensions(tile);
            let (first_column, first_row) = ((tile % 3) * 2, (tile / 3) * 2);
            (0..rows)
                .flat_map(|row| {
                    (0..columns).map(move |column| {
                        let (column, row) = (first_column + column, first_row + row);
                        if (column, row) == (1, 1) {
                            f64::NAN
                        } else {
                            (row * 10 + column) as f64 + 0.5
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let dir = std::env::temp_dir().join(format!("netweevil-elevation-{}", std::process::id()));
    let path = dir.join("raster.bin");
    write_elevation_raster_sectioned(&path, &header, &tiles).unwrap();
    let reader = ElevationRasterReader::open(&path).unwrap();
    assert_eq!(reader.header(), &header);
    let (tile, offset) = header.cell_tile(3, 1);
    assert_eq!(reader.tile(tile).unwrap()[offset], 13.5);
    let (tile, offset) = header.cell_tile(1, 1);
    assert!(reader.tile(tile).unwrap()[offset].is_nan());
    assert_eq!(reader.tile(5).unwrap(), vec![24.5]);
    assert!(reader.tile(6).is_err());
    assert!(write_elevation_raster_sectioned(&path, &header, &tiles[1..]).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
