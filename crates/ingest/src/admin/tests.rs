use super::*;
use crate::test_util::TempDir;

/// Square ring through `(lon, lat)` corners, closed.
fn square(min: (f64, f64), max: (f64, f64)) -> Vec<(f64, f64)> {
    vec![
        (min.0, min.1),
        (max.0, min.1),
        (max.0, max.1),
        (min.0, max.1),
        (min.0, min.1),
    ]
}

fn boundary(level: u8, code: &str, rings: Vec<Vec<(f64, f64)>>) -> AdminBoundary {
    AdminBoundary {
        level,
        code: Some(code.to_string()),
        name: Some(format!("{code} name")),
        rings,
    }
}

fn candidate(relation_id: i64, member_way_ids: &[i64]) -> AdminRelationCandidate {
    AdminRelationCandidate {
        relation_id,
        level: STATE_LEVEL,
        code: Some("NL-GR".to_string()),
        name: None,
        member_way_ids: member_way_ids.to_vec(),
    }
}

#[test]
fn rings_close_from_shuffled_and_reversed_member_ways() {
    // A square 1-2-3-4 split into three ways listed out of order, one reversed.
    let way_nodes = FxHashMap::from_iter([(10, vec![1, 2]), (11, vec![4, 3, 2]), (12, vec![4, 1])]);
    let node_coords = FxHashMap::from_iter([
        (1, (0.0, 0.0)),
        (2, (1.0, 0.0)),
        (3, (1.0, 1.0)),
        (4, (0.0, 1.0)),
    ]);
    let (boundaries, incomplete) =
        assemble_boundaries(&[candidate(7, &[12, 10, 11])], &way_nodes, &node_coords);
    assert!(incomplete.is_empty());
    assert_eq!(boundaries.len(), 1);
    let ring = &boundaries[0].rings[0];
    assert_eq!(ring.len(), 5);
    assert_eq!(ring.first(), ring.last());
}

#[test]
fn relations_with_missing_ways_nodes_or_open_rings_are_dropped() {
    let way_nodes = FxHashMap::from_iter([(10, vec![1, 2, 3]), (11, vec![3, 4])]);
    let node_coords = FxHashMap::from_iter([
        (1, (0.0, 0.0)),
        (2, (1.0, 0.0)),
        (3, (1.0, 1.0)),
        (4, (0.0, 1.0)),
    ]);
    let (boundaries, incomplete) = assemble_boundaries(
        &[
            candidate(1, &[10, 11]),
            candidate(2, &[10, 99]),
            candidate(3, &[10]),
        ],
        &way_nodes,
        &node_coords,
    );
    assert!(boundaries.is_empty());
    assert_eq!(incomplete, vec![1, 2, 3]);
}

#[test]
fn even_odd_rule_excludes_holes_and_smallest_boundary_wins() {
    let mut country = square((0.0, 0.0), (10.0, 10.0));
    country.reverse();
    let index = AdminIndex::new(vec![
        boundary(
            COUNTRY_LEVEL,
            "NL",
            vec![country, square((4.0, 4.0), (6.0, 6.0))],
        ),
        // An enclave of another country filling the hole.
        boundary(COUNTRY_LEVEL, "BE", vec![square((4.0, 4.0), (6.0, 6.0))]),
        boundary(STATE_LEVEL, "NL-GR", vec![square((0.0, 0.0), (3.0, 3.0))]),
    ]);
    let area = index.lookup(1.0, 1.0).unwrap();
    assert_eq!(area.country_code, "NL");
    assert_eq!(area.state_code.as_deref(), Some("NL-GR"));
    assert_eq!(area.country_name.as_deref(), Some("NL name"));
    assert_eq!(index.lookup(5.0, 5.0).unwrap().country_code, "BE");
    assert_eq!(index.lookup(8.0, 8.0).unwrap().state_code, None);
    assert!(index.lookup(11.0, 5.0).is_none());
}

#[test]
fn a_state_without_its_country_boundary_supplies_the_country_code() {
    let index = AdminIndex::new(vec![boundary(
        STATE_LEVEL,
        "NL-GR",
        vec![square((6.0, 53.0), (7.0, 53.5))],
    )]);
    let area = index.lookup(6.5, 53.2).unwrap();
    assert_eq!(area.country_code, "NL");
    assert_eq!(area.country_name, None);
    assert_eq!(area.state_name.as_deref(), Some("NL-GR name"));
}

#[test]
fn assignment_interns_areas_per_node() {
    let index = AdminIndex::new(vec![
        boundary(COUNTRY_LEVEL, "NL", vec![square((0.0, 0.0), (1.0, 1.0))]),
        boundary(COUNTRY_LEVEL, "DE", vec![square((1.0, 0.0), (2.0, 1.0))]),
    ]);
    let nodes = [(0.5, 0.5), (1.5, 0.5), (0.2, 0.2), (5.0, 5.0)]
        .iter()
        .enumerate()
        .map(|(id, &(lon, lat))| TopologyNode {
            node_id: netweevil_core::NodeId(id as u32),
            lon,
            lat,
            z: 0.0,
        })
        .collect::<Vec<_>>();
    let (areas, indexes) = index.assign(&nodes);
    assert_eq!(areas.len(), 2);
    assert_eq!(indexes, vec![0, 1, 0, NO_ADMIN_AREA]);
    assert_eq!(areas[1].country_code, "DE");
}

#[test]
fn geojson_boundaries_infer_levels_and_codes() {
    let dir = TempDir::new("admin-geojson");
    let path = dir.join("boundaries.geojson");
    std::fs::write(
        &path,
        r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"ISO_A2":"-99","ISO_A2_EH":"FR","NAME":"France"},
           "geometry":{"type":"MultiPolygon","coordinates":[[[[0,0],[2,0],[2,2],[0,2],[0,0]]]]}},
          {"type":"Feature","properties":{"iso_3166_2":"FR-IDF","name":"Ile-de-France"},
           "geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1],[0,0]]]}},
          {"type":"Feature","properties":{"admin_level":8,"name":"Paris"},
           "geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}
        ]}"#,
    )
    .unwrap();
    let (boundaries, incomplete) = read_boundaries_file(&path).unwrap();
    assert!(incomplete.is_empty());
    assert_eq!(boundaries.len(), 2);
    assert_eq!(boundaries[0].level, COUNTRY_LEVEL);
    assert_eq!(boundaries[0].code.as_deref(), Some("FR"));
    assert_eq!(boundaries[1].level, STATE_LEVEL);
    assert_eq!(boundaries[1].code.as_deref(), Some("FR-IDF"));
    let area = AdminIndex::new(boundaries).lookup(0.5, 0.5).unwrap();
    assert_eq!(area.country_name.as_deref(), Some("France"));
    assert_eq!(area.state_code.as_deref(), Some("FR-IDF"));

    std::fs::write(&path, r#"{"type":"Point","coordinates":[0,0]}"#).unwrap();
    assert!(read_boundaries_file(&path).is_err());
}

#[test]
fn osm_relations_parse_country_and_state_codes() {
    use osmpbfreader::{Ref, RelationId, Tags, WayId};
    let relation = |tags: &[(&str, &str)], role: &str| Relation {
        id: RelationId(5),
        tags: Tags::from_iter(
            tags.iter()
                .map(|(key, value)| ((*key).into(), (*value).into())),
        ),
        refs: vec![
            Ref {
                member: OsmId::Way(WayId(9)),
                role: role.into(),
            },
            Ref {
                member: OsmId::Node(osmpbfreader::NodeId(1)),
                role: "admin_centre".into(),
            },
        ],
    };
    let country = parse_admin_relation(&relation(
        &[
            ("boundary", "administrative"),
            ("admin_level", "2"),
            ("ISO3166-1", "nl"),
            ("name", "Nederland"),
        ],
        "outer",
    ))
    .unwrap();
    assert_eq!(country.code.as_deref(), Some("NL"));
    assert_eq!(country.member_way_ids, vec![9]);
    assert!(
        parse_admin_relation(&relation(
            &[("boundary", "administrative"), ("admin_level", "8")],
            "outer"
        ))
        .is_none()
    );
    assert!(
        parse_admin_relation(&relation(
            &[("boundary", "administrative"), ("admin_level", "4")],
            "subarea"
        ))
        .is_none()
    );
}
