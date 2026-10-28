use super::*;
use std::path::Path;

use rustc_hash::FxHashMap;

use crate::scan::{ObjectCounts, ScanOutput};
use crate::test_util::pending_way;

fn tags(pairs: &[(&str, &str)]) -> Tags {
    Tags::from_iter(pairs.iter().map(|&(key, value)| (key.into(), value.into())))
}

fn lanes(tags: &WayGuidanceTags) -> (Option<&str>, Option<&str>) {
    (
        tags.forward.turn_lanes.as_deref(),
        tags.backward.turn_lanes.as_deref(),
    )
}

#[test]
fn unsuffixed_tags_apply_only_to_the_single_direction_of_a_one_way_road() {
    let way = [("turn:lanes", "left|through"), ("ref", "N355")];
    let two_way = [
        ("turn:lanes:forward", "left|through"),
        ("turn:lanes:backward", "through|right"),
    ];
    for (pairs, forward_oneway, backward_oneway, expected) in [
        (&way[..], true, false, (Some("left|through"), None)),
        (&way[..], false, true, (None, Some("left|through"))),
        (&way[..], false, false, (None, None)),
        (
            &two_way[..],
            false,
            false,
            (Some("left|through"), Some("through|right")),
        ),
    ] {
        let guidance = way_guidance_tags(&tags(pairs), forward_oneway, backward_oneway);
        let guidance = guidance.as_deref();
        assert_eq!(
            guidance.map(lanes).unwrap_or_default(),
            expected,
            "{pairs:?} oneway=({forward_oneway}, {backward_oneway})"
        );
    }
}

#[test]
fn ways_without_guidance_tags_store_nothing() {
    assert!(way_guidance_tags(&tags(&[("name", "Hereweg")]), true, false).is_none());
    assert!(way_guidance_tags(&tags(&[("ref", "  ")]), true, false).is_none());
    assert!(junction_node_tags(&tags(&[("highway", "traffic_signals")])).is_none());
    assert!(junction_node_tags(&tags(&[("highway", "motorway_junction")])).is_none());
}

#[test]
fn destinations_follow_the_travel_direction() {
    let guidance = way_guidance_tags(
        &tags(&[
            ("destination", "Groningen;Assen"),
            ("destination:ref", "A28"),
            ("destination:street:backward", "Europaweg"),
            ("junction:ref", "36"),
            ("exit_to", "Haren"),
        ]),
        true,
        false,
    )
    .unwrap();
    assert_eq!(
        guidance.forward.destination.as_deref(),
        Some("Groningen;Assen")
    );
    assert_eq!(guidance.forward.destination_ref.as_deref(), Some("A28"));
    assert_eq!(guidance.forward.destination_street, None);
    assert_eq!(
        guidance.backward.destination_street.as_deref(),
        Some("Europaweg")
    );
    assert_eq!(guidance.junction_ref.as_deref(), Some("36"));
    assert_eq!(guidance.exit_to.as_deref(), Some("Haren"));
}

#[test]
fn junction_exit_numbers_attach_to_link_roads_and_names_to_every_branch() {
    let junction = junction_node_tags(&tags(&[
        ("highway", "motorway_junction"),
        ("ref", "36"),
        ("name", "Julianaplein"),
        ("exit_to", "Centrum"),
    ]))
    .unwrap();
    let mut interner = GuidanceInterner::new();
    interner.push_edge(None, true, HighwayClass::Motorway, Some(&junction));
    interner.push_edge(None, true, HighwayClass::MotorwayLink, Some(&junction));
    interner.push_edge(None, true, HighwayClass::MotorwayLink, Some(&junction));
    interner.push_edge(None, true, HighwayClass::Residential, None);
    let bundle = interner.finish(vec!["Hereweg".into()]);

    let main = bundle.guidance(0);
    let link = bundle.guidance(1);
    assert_eq!(bundle.string(main.junction_name), Some("Julianaplein"));
    assert_eq!(main.junction_ref, None);
    assert_eq!(bundle.string(link.junction_ref), Some("36"));
    assert_eq!(bundle.string(link.exit_to), Some("Centrum"));
    // Identical records are shared and the empty record is index 0.
    assert_eq!(bundle.edge_guidance, [1, 2, 2, 0]);
    assert_eq!(bundle.strings, ["36", "Centrum", "Julianaplein"]);
    assert_eq!(bundle.validate(Some(4)), Ok(()));
}

#[test]
fn datasets_without_guidance_tags_get_empty_columns() {
    let mut interner = GuidanceInterner::new();
    interner.push_edge(None, true, HighwayClass::Residential, None);
    let bundle = interner.finish(vec!["Hereweg".into()]);
    assert!(bundle.edge_guidance.is_empty() && bundle.guidance_records.is_empty());
    assert!(bundle.guidance(0).is_empty());
}

#[test]
fn topology_build_keeps_lanes_per_direction_and_exit_numbers_on_links() {
    // Way 10 is a two-way road 100 -> 200 with direction-specific lanes;
    // way 20 is a one-way link leaving junction node 200.
    let mut road = pending_way(10, &[100, 200]);
    road.guidance = way_guidance_tags(
        &tags(&[
            ("ref", "N355"),
            ("turn:lanes:forward", "left|through"),
            ("turn:lanes:backward", "through|right"),
        ]),
        false,
        false,
    );
    let mut link = pending_way(20, &[200, 300]);
    link.highway = HighwayClass::MotorwayLink;
    link.reverse_access_mask = netweevil_core::AccessMask::new(0);
    let scan = ScanOutput {
        pending_ways: vec![road, link],
        node_coords: FxHashMap::from_iter([
            (100, (6.0, 53.0, 0.0)),
            (200, (6.001, 53.0, 0.0)),
            (300, (6.002, 53.0, 0.0)),
        ]),
        junction_nodes: FxHashMap::from_iter([(
            200,
            JunctionNodeTags {
                reference: Some("36".into()),
                ..JunctionNodeTags::default()
            },
        )]),
        counts: ObjectCounts::default(),
        ..ScanOutput::default()
    };
    let (topology, names, _) =
        crate::topology::build_topology_from_scan(Path::new("test.pbf"), scan, &mut |_| {})
            .unwrap();
    assert_eq!(topology.edge_count(), 3);
    assert_eq!(names.validate(Some(3)), Ok(()));
    let lane_string = |edge| names.string(names.guidance(edge).turn_lanes);
    assert_eq!(lane_string(0), Some("left|through"));
    assert_eq!(lane_string(1), Some("through|right"));
    assert_eq!(names.string(names.guidance(0).reference), Some("N355"));
    assert_eq!(names.string(names.guidance(2).junction_ref), Some("36"));
}
