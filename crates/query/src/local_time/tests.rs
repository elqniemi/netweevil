use super::*;
use netweevil_core::{
    NodeId, NodeSpatialIndex, RoutingEdge, TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyEdgeLayers,
    TopologyNode,
};

fn at(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &Rfc3339).unwrap()
}

/// Two nodes in Amsterdam and one in Berlin, edges 0 (0->1) and 1 (2->0),
/// plus an unknown zone name on node 3.
fn zoned_topology() -> TopologyBundle {
    let nodes = [(6.56, 53.21), (6.57, 53.21), (7.45, 53.23), (7.46, 53.23)]
        .iter()
        .enumerate()
        .map(|(id, &(lon, lat))| TopologyNode {
            node_id: NodeId(id as u32),
            lon,
            lat,
            z: 0.0,
        })
        .collect::<Vec<_>>();
    let routing = [(0, 1), (2, 0)]
        .map(|(from, to)| RoutingEdge {
            from: NodeId(from),
            to: NodeId(to),
            ..RoutingEdge::default()
        })
        .to_vec();
    let mut topology = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "zones".to_string(),
        source_sha256: "abc".to_string(),
        spatial_index: NodeSpatialIndex::build(&nodes),
        nodes,
        edge_layers: TopologyEdgeLayers {
            profile: vec![Default::default(); routing.len()],
            presentation: vec![Default::default(); routing.len()],
            routing,
        },
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: Default::default(),
        node_component_ids: Vec::new(),
        edge_component_ids: Vec::new(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        regions: Default::default(),
        elevation: Default::default(),
    };
    topology.regions.timezones = vec![
        "Europe/Amsterdam".to_string(),
        "Europe/Berlin".to_string(),
        "Mars/Olympus_Mons".to_string(),
    ];
    topology.regions.node_timezone = vec![0, 0, 1, 2];
    topology
}

#[test]
fn instants_keep_their_moment_and_take_the_zone_offset() {
    let amsterdam = chrono_tz::Europe::Amsterdam;
    // Clocks go back at 01:00 UTC on 2026-10-25.
    let before = in_zone(at("2026-10-25T00:30:00Z"), amsterdam);
    let after = in_zone(at("2026-10-25T01:30:00Z"), amsterdam);
    assert_eq!(before, at("2026-10-25T00:30:00Z"));
    assert_eq!((before.hour(), before.offset().whole_hours()), (2, 2));
    assert_eq!((after.hour(), after.offset().whole_hours()), (2, 1));
}

#[test]
fn edges_read_the_zone_of_their_tail_node() {
    let topology = zoned_topology();
    let zones = NetworkTimezones::new(&topology);
    assert_eq!(
        zones.edge_zone(&topology, 0),
        Some(chrono_tz::Europe::Amsterdam)
    );
    assert_eq!(
        zones.edge_zone(&topology, 1),
        Some(chrono_tz::Europe::Berlin)
    );
    assert_eq!(
        zones.node_zone(&topology, 3),
        None,
        "unknown zone names stay unset"
    );
    let instant = at("2026-07-10T08:00:00Z");
    assert_eq!(zones.edge_local_time(&topology, 0, instant).hour(), 10);
    assert_eq!(zones.edge_local_time(&topology, 7, instant), instant);
    assert_eq!(
        zones.zone_at(&topology, 7.44, 53.22),
        Some(chrono_tz::Europe::Berlin)
    );
    assert_eq!(
        zones.zone_at(&topology, 6.0, 53.0),
        Some(chrono_tz::Europe::Amsterdam)
    );

    let mut single = zoned_topology();
    single.regions.node_timezone = vec![1; 4];
    let zones = NetworkTimezones::new(&single);
    assert_eq!(
        zones.zone_at(&single, 0.0, 0.0),
        Some(chrono_tz::Europe::Berlin)
    );
    assert_eq!(NetworkTimezones::default().zone_at(&single, 0.0, 0.0), None);
}

#[test]
fn local_datetimes_follow_the_transit_dst_policy() {
    let zone = Some(chrono_tz::Europe::Amsterdam);
    assert_eq!(
        parse_local_datetime("2026-07-10T10:00", zone).unwrap(),
        at("2026-07-10T10:00:00+02:00")
    );
    assert_eq!(
        parse_local_datetime("2026-01-05", zone).unwrap(),
        at("2026-01-05T00:00:00+01:00")
    );
    assert_eq!(
        parse_local_datetime(" 2026-07-10T10:00:00-04:00 ", None).unwrap(),
        at("2026-07-10T14:00:00Z")
    );
    for (raw, message) in [
        ("2026-03-29T02:30:00", "does not exist"),
        ("2026-10-25T02:30:00", "ambiguous"),
        ("2026-10-25 02:30", "invalid datetime"),
    ] {
        let error = parse_local_datetime(raw, zone).unwrap_err();
        assert!(error.to_string().contains(message), "{raw}: {error}");
    }
    let error = parse_local_datetime("2026-07-10T10:00", None).unwrap_err();
    assert!(error.to_string().contains("explicit offset"), "{error}");
}
