use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::scan::PendingWay;
use crate::topology::build_edge_based_topology;
use netweevil_core::{
    AccessMask, DirectedEdge, EdgeId, HighwayClass, NO_FEATURE_ROW, NodeId, RoadClass,
    SurfaceClass, TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyEdgeLayers, TopologyNode,
};
use osmpbfreader::OsmObj;

mod osc;
mod pbf;

pub(crate) use osc::{OsmEdit, apply_edits, write_osc};
pub(crate) use pbf::write_pbf;

/// Side of [`osm_grid`] in nodes.
pub(crate) const GRID_SIDE: i64 = 8;

/// Node id of grid position `(row, col)`.
pub(crate) fn grid_node(row: i64, col: i64) -> i64 {
    row * GRID_SIDE + col + 1
}

/// Decimicro position of grid position `(row, col)`, about 70 by 110 m apart.
fn grid_position(row: i64, col: i64) -> (i32, i32) {
    (
        65_000_000 + col as i32 * 10_000,
        532_000_000 + row as i32 * 10_000,
    )
}

/// An 8 by 8 street grid in OSM object form, sorted like a PBF extract:
/// residential row ways `100 + row` and column ways `200 + col`, traffic
/// signals at two crossings, a no-left-turn restriction (relation 500), a
/// building (way 900 over nodes 1000 to 1003) and an unrelated relation.
pub(crate) fn osm_grid() -> Vec<OsmObj> {
    let mut objects = Vec::new();
    for row in 0..GRID_SIDE {
        for col in 0..GRID_SIDE {
            let (lon, lat) = grid_position(row, col);
            let tags: &[(&str, &str)] = match (row, col) {
                (2, 2) | (5, 4) => &[("highway", "traffic_signals")],
                (0, 6) => &[("highway", "motorway_junction"), ("ref", "12")],
                _ => &[],
            };
            objects.push(osm_node(grid_node(row, col), lon, lat, tags));
        }
    }
    for (offset, (lon, lat)) in [(0, 0), (0, 5_000), (5_000, 5_000), (5_000, 0)]
        .into_iter()
        .enumerate()
    {
        objects.push(osm_node(
            1000 + offset as i64,
            65_120_000 + lon,
            532_120_000 + lat,
            &[],
        ));
    }
    for row in 0..GRID_SIDE {
        let nodes = (0..GRID_SIDE)
            .map(|col| grid_node(row, col))
            .collect::<Vec<_>>();
        let name = format!("Row {row}");
        objects.push(osm_way(
            100 + row,
            &nodes,
            &[("highway", "residential"), ("name", &name)],
        ));
    }
    for col in 0..GRID_SIDE {
        let nodes = (0..GRID_SIDE)
            .map(|row| grid_node(row, col))
            .collect::<Vec<_>>();
        let tags: &[(&str, &str)] = match col {
            3 => &[("highway", "secondary"), ("ref", "N360")],
            6 => &[
                ("highway", "primary_link"),
                ("oneway", "yes"),
                ("destination", "Centrum"),
                ("turn:lanes", "left|through"),
            ],
            _ => &[("highway", "residential")],
        };
        objects.push(osm_way(200 + col, &nodes, tags));
    }
    objects.push(osm_way(
        900,
        &[1000, 1001, 1002, 1003, 1000],
        &[("building", "yes")],
    ));
    objects.push(osm_turn_restriction(
        500,
        "no_left_turn",
        102,
        grid_node(2, 3),
        203,
    ));
    objects.push(OsmObj::Relation(osmpbfreader::Relation {
        id: osmpbfreader::RelationId(600),
        tags: osm_tags(&[("type", "route"), ("route", "bus")]),
        refs: Vec::new(),
    }));
    objects
}

/// A realistic day of edits to [`osm_grid`]: a tag and speed change, a
/// closed road, a new road over new nodes, a moved node, a way split, a
/// node removed from a way and deleted, a new signal, a new restriction,
/// a building turned into a service road over nodes that were not part of
/// the topology, and a restriction relation that becomes a route.
pub(crate) fn osm_grid_edits() -> Vec<OsmEdit> {
    use crate::osm_change::ChangeAction::{Create, Delete, Modify};
    let row = |row: i64, cols: std::ops::RangeInclusive<i64>| {
        cols.map(|col| grid_node(row, col)).collect::<Vec<_>>()
    };
    let (moved_lon, moved_lat) = grid_position(2, 5);
    let (signal_lon, signal_lat) = grid_position(4, 4);
    let (corner_lon, corner_lat) = grid_position(7, 7);
    let (junction_lon, junction_lat) = grid_position(0, 6);
    let seventh_column = (0..GRID_SIDE - 1)
        .map(|row| grid_node(row, 7))
        .collect::<Vec<_>>();
    vec![
        (
            Modify,
            osm_way(
                103,
                &row(3, 0..=7),
                &[
                    ("highway", "primary"),
                    ("maxspeed", "80"),
                    ("name", "Row 3"),
                ],
            ),
        ),
        (Delete, osm_way(205, &[], &[])),
        (Create, osm_node(5001, 65_015_000, 532_015_000, &[])),
        (Create, osm_node(5002, 65_025_000, 532_025_000, &[])),
        (
            Create,
            osm_way(
                300,
                &[grid_node(1, 1), 5001, 5002, grid_node(3, 3)],
                &[("highway", "tertiary"), ("oneway", "yes")],
            ),
        ),
        (
            Modify,
            osm_node(grid_node(2, 5), moved_lon + 1_234, moved_lat - 987, &[]),
        ),
        (
            Modify,
            osm_way(
                101,
                &row(1, 0..=4),
                &[("highway", "residential"), ("name", "Row 1")],
            ),
        ),
        (
            Create,
            osm_way(
                301,
                &row(1, 4..=7),
                &[("highway", "residential"), ("name", "Row 1")],
            ),
        ),
        (
            Modify,
            osm_way(207, &seventh_column, &[("highway", "residential")]),
        ),
        (
            Modify,
            osm_way(
                107,
                &row(7, 0..=6),
                &[("highway", "residential"), ("name", "Row 7")],
            ),
        ),
        (
            Delete,
            osm_node(grid_node(7, 7), corner_lon, corner_lat, &[]),
        ),
        (
            Modify,
            osm_way(
                206,
                &(0..GRID_SIDE)
                    .map(|row| grid_node(row, 6))
                    .collect::<Vec<_>>(),
                &[
                    ("highway", "primary_link"),
                    ("oneway", "yes"),
                    ("destination", "Centrum;Station"),
                    ("turn:lanes", "left|left;through"),
                ],
            ),
        ),
        (
            Modify,
            osm_node(
                grid_node(0, 6),
                junction_lon,
                junction_lat,
                &[("highway", "motorway_junction"), ("ref", "12a")],
            ),
        ),
        (
            Modify,
            osm_node(
                grid_node(4, 4),
                signal_lon,
                signal_lat,
                &[("highway", "traffic_signals")],
            ),
        ),
        (
            Create,
            osm_turn_restriction(501, "only_straight_on", 104, grid_node(4, 2), 104),
        ),
        (
            Modify,
            osm_way(900, &[1000, 1001, 1002, 1003], &[("highway", "service")]),
        ),
        (
            Modify,
            OsmObj::Relation(osmpbfreader::Relation {
                id: osmpbfreader::RelationId(500),
                tags: osm_tags(&[("type", "route"), ("route", "bicycle")]),
                refs: Vec::new(),
            }),
        ),
    ]
}

fn osm_tags(tags: &[(&str, &str)]) -> osmpbfreader::Tags {
    tags.iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

/// OSM node at decimicro-degree coordinates.
pub(crate) fn osm_node(id: i64, lon_e7: i32, lat_e7: i32, tags: &[(&str, &str)]) -> OsmObj {
    OsmObj::Node(osmpbfreader::Node {
        id: osmpbfreader::NodeId(id),
        tags: osm_tags(tags),
        decimicro_lat: lat_e7,
        decimicro_lon: lon_e7,
    })
}

pub(crate) fn osm_way(id: i64, nodes: &[i64], tags: &[(&str, &str)]) -> OsmObj {
    OsmObj::Way(osmpbfreader::Way {
        id: osmpbfreader::WayId(id),
        tags: osm_tags(tags),
        nodes: nodes
            .iter()
            .map(|node| osmpbfreader::NodeId(*node))
            .collect(),
    })
}

/// `type=restriction` relation from one way over a via node to another.
pub(crate) fn osm_turn_restriction(
    id: i64,
    restriction: &str,
    from_way: i64,
    via_node: i64,
    to_way: i64,
) -> OsmObj {
    let member = |member: osmpbfreader::OsmId, role: &str| osmpbfreader::Ref {
        member,
        role: role.into(),
    };
    OsmObj::Relation(osmpbfreader::Relation {
        id: osmpbfreader::RelationId(id),
        tags: osm_tags(&[("type", "restriction"), ("restriction", restriction)]),
        refs: vec![
            member(
                osmpbfreader::OsmId::Way(osmpbfreader::WayId(from_way)),
                "from",
            ),
            member(
                osmpbfreader::OsmId::Node(osmpbfreader::NodeId(via_node)),
                "via",
            ),
            member(osmpbfreader::OsmId::Way(osmpbfreader::WayId(to_way)), "to"),
        ],
    })
}

pub(crate) fn pending_way(osm_way_id: i64, node_ids: &[i64]) -> PendingWay {
    PendingWay {
        osm_way_id,
        node_ids: node_ids.to_vec(),
        road_class: RoadClass::Residential,
        highway: HighwayClass::Residential,
        duration_s: None,
        surface: SurfaceClass::Asphalt,
        smoothness: Default::default(),
        forward_access_mask: AccessMask::new(AccessMask::CAR),
        reverse_access_mask: AccessMask::new(AccessMask::CAR),
        is_toll: false,
        is_roundabout: false,
        forward_extra_flags: 0,
        reverse_extra_flags: 0,
        forward_max_speed_kph: None,
        reverse_max_speed_kph: None,
        forward_lanes: None,
        reverse_lanes: None,
        name: None,
        guidance: None,
        feature_row: NO_FEATURE_ROW,
        temporal_rule_id: None,
    }
}

pub(crate) fn edge(edge_id: u32, from: u32, to: u32, source_way_id: i64) -> DirectedEdge {
    DirectedEdge {
        edge_id: EdgeId(edge_id),
        from: NodeId(from),
        to: NodeId(to),
        source_way_id,
        length_m: 100,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: NO_FEATURE_ROW,
        source_direction: 1,
        temporal_rule_id: None,
        duration_s: None,
        road_class: RoadClass::Residential,
        surface: SurfaceClass::Asphalt,
        smoothness: Default::default(),
        access_mask: AccessMask::new(AccessMask::CAR),
        is_toll: false,
        max_speed_kph: None,
        lanes: None,
        name_index: None,
        geometry_offset: 0,
        geometry_len: 0,
        flags: 0,
    }
}

/// Single-component topology over `(lon, lat, z)` nodes with its
/// edge-based transition graph built from `edges`.
pub(crate) fn topology(nodes: &[(f64, f64, f64)], edges: &[DirectedEdge]) -> TopologyBundle {
    TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        nodes: nodes
            .iter()
            .enumerate()
            .map(|(node_id, &(lon, lat, z))| TopologyNode {
                node_id: NodeId(node_id as u32),
                lon,
                lat,
                z,
            })
            .collect(),
        edge_layers: TopologyEdgeLayers::from_directed_edges(edges),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: build_edge_based_topology(nodes.len(), edges),
        spatial_index: None,
        node_component_ids: vec![0; nodes.len()],
        edge_component_ids: vec![0; edges.len()],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    }
}

/// Fresh directory under the system temp dir, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "netweevil-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir creates");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// GeoPackage binary header (EPSG:4326, no envelope) followed by a
/// little-endian WKB LineString Z.
pub(crate) fn gpkg_linestring_z(points: &[[f64; 3]]) -> Vec<u8> {
    let mut blob = b"GP\0\x01".to_vec();
    blob.extend_from_slice(&4326_i32.to_le_bytes());
    blob.push(1);
    blob.extend_from_slice(&1002_u32.to_le_bytes());
    blob.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for value in points.iter().flatten() {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}
