//! Maneuver, lane, signpost and phrase tests on small synthetic junctions.
//! Networks are laid out in metres east/north of 6°E 53°N.

use std::collections::BTreeMap;

use netweevil_core::{
    DirectedEdge, EDGE_FLAG_ROUNDABOUT, EdgeGuidance, EdgeNameBundle, FeatureAttributeColumn,
    FeatureAttributeColumnData, FeatureAttributeDefinition, FeatureAttributeTable,
    FeatureAttributeType, HighwayClass, TopologyBundle,
};

use super::*;
use crate::tests::fixtures::*;
use crate::{EngineMode, PreparedRoutingEngine, RouteRequest};

const METERS_PER_DEGREE: f64 = 111_320.0;

fn lonlat([x, y]: [f64; 2]) -> (f64, f64) {
    (
        6.0 + x / (METERS_PER_DEGREE * 53_f64.to_radians().cos()),
        53.0 + y / METERS_PER_DEGREE,
    )
}

#[derive(Clone, Copy, Default)]
struct Way<'a> {
    name: Option<&'a str>,
    reference: Option<&'a str>,
    highway: Option<HighwayClass>,
    oneway: bool,
    roundabout: bool,
    /// Forward direction only.
    turn_lanes: Option<&'a str>,
    destination: Option<&'a str>,
    destination_ref: Option<&'a str>,
    junction_ref: Option<&'a str>,
    feature_row: Option<u32>,
}

fn named(name: &str) -> Way<'_> {
    Way {
        name: Some(name),
        ..Way::default()
    }
}

/// A synthetic network: points in metres, ways over point indexes.
struct Net {
    points: Vec<[f64; 3]>,
    edges: Vec<DirectedEdge>,
    highways: Vec<HighwayClass>,
    names: Vec<String>,
    strings: Vec<String>,
    records: Vec<EdgeGuidance>,
    feature_attributes: FeatureAttributeTable,
}

impl Net {
    fn new(points: &[[f64; 2]]) -> Self {
        Self {
            points: points.iter().map(|&[x, y]| [x, y, 0.0]).collect(),
            edges: Vec::new(),
            highways: Vec::new(),
            names: Vec::new(),
            strings: Vec::new(),
            records: vec![EdgeGuidance::default()],
            feature_attributes: FeatureAttributeTable::default(),
        }
    }

    fn with_z(mut self, point: usize, z: f64) -> Self {
        self.points[point][2] = z;
        self
    }

    fn string(&mut self, value: Option<&str>) -> Option<u32> {
        let value = value?;
        self.strings.push(value.to_string());
        Some(self.strings.len() as u32 - 1)
    }

    fn way(mut self, nodes: &[usize], way: Way<'_>) -> Self {
        let highway = way.highway.unwrap_or(HighwayClass::Residential);
        let name_index = way.name.map(|name| {
            self.names.push(name.to_string());
            self.names.len() as u32 - 1
        });
        let forward = EdgeGuidance {
            reference: self.string(way.reference),
            turn_lanes: self.string(way.turn_lanes),
            destination: self.string(way.destination),
            destination_ref: self.string(way.destination_ref),
            junction_ref: self.string(way.junction_ref),
            ..EdgeGuidance::default()
        };
        let backward = EdgeGuidance {
            reference: forward.reference,
            ..EdgeGuidance::default()
        };
        for pair in nodes.windows(2) {
            let directions: &[(usize, usize, EdgeGuidance)] = if way.oneway {
                &[(pair[0], pair[1], forward)]
            } else {
                &[(pair[0], pair[1], forward), (pair[1], pair[0], backward)]
            };
            for &(from, to, guidance) in directions {
                let [ax, ay, _] = self.points[from];
                let [bx, by, _] = self.points[to];
                let length = (ax - bx).hypot(ay - by).round().max(1.0) as u32;
                let id = self.edges.len() as u32;
                self.edges.push(DirectedEdge {
                    road_class: highway.road_class(),
                    name_index,
                    flags: if way.roundabout {
                        EDGE_FLAG_ROUNDABOUT
                    } else {
                        0
                    },
                    feature_row: way.feature_row.unwrap_or(netweevil_core::NO_FEATURE_ROW),
                    ..edge(id, from as u32, to as u32, i64::from(id), length)
                });
                self.highways.push(highway);
                self.records.push(guidance);
            }
        }
        self
    }

    fn topology(&self) -> TopologyBundle {
        let coordinates = self
            .points
            .iter()
            .map(|&[x, y, _]| lonlat([x, y]))
            .collect::<Vec<_>>();
        let mut topology = topology("navigation", &coordinates, &self.edges);
        for (profile, highway) in topology.edge_layers.profile.iter_mut().zip(&self.highways) {
            profile.highway = *highway;
        }
        for (node, point) in topology.nodes.iter_mut().zip(&self.points) {
            node.z = point[2];
        }
        topology.feature_attributes = self.feature_attributes.clone();
        topology
    }

    fn names(&self) -> EdgeNameBundle {
        EdgeNameBundle {
            names: self.names.clone(),
            strings: self.strings.clone(),
            guidance_records: self.records.clone(),
            edge_guidance: (0..self.edges.len() as u32).map(|edge| edge + 1).collect(),
        }
    }

    fn engine(&self) -> PreparedRoutingEngine {
        // 10 m/s everywhere, so the shortest path is the fastest.
        let costs = self
            .edges
            .iter()
            .map(|edge| f64::from(edge.length_m) / 10.0)
            .collect::<Vec<_>>();
        exact_engine(self.topology(), metrics(&costs))
    }

    fn request(&self, from: [f64; 2], to: [f64; 2]) -> RouteRequest {
        let (from, to) = (lonlat(from), lonlat(to));
        RouteRequest {
            snap: snap_within(30.0),
            ..route_request(
                "directions",
                point("a", from.0, from.1),
                point("b", to.0, to.1),
            )
        }
    }

    fn directions_with(
        &self,
        from: [f64; 2],
        to: [f64; 2],
        options: &DirectionsOptions,
    ) -> DirectionsResult {
        let names = self.names();
        self.engine()
            .execute_directions(
                &self.request(from, to),
                DirectionsContext::new(&names),
                options,
                EngineMode::Auto,
            )
            .expect("directions succeed")
    }

    fn directions(&self, from: [f64; 2], to: [f64; 2]) -> DirectionsResult {
        self.directions_with(from, to, &DirectionsOptions::default())
    }
}

fn kinds(result: &DirectionsResult) -> Vec<ManeuverType> {
    result
        .maneuvers
        .iter()
        .map(|maneuver| maneuver.kind)
        .collect()
}

fn instructions(result: &DirectionsResult) -> Vec<&str> {
    result
        .maneuvers
        .iter()
        .map(|maneuver| maneuver.instruction.as_str())
        .collect()
}

fn in_language(language: &str) -> DirectionsOptions {
    DirectionsOptions {
        language: Some(language.into()),
        ..DirectionsOptions::default()
    }
}

/// Main runs south-north and Cross west-east through the centre (point 0).
fn crossroads(approach: Way<'_>) -> Net {
    Net::new(&[
        [0.0, 0.0],
        [0.0, -200.0],
        [0.0, 200.0],
        [200.0, 0.0],
        [-200.0, 0.0],
    ])
    .way(&[1, 0], approach)
    .way(&[0, 2], named("Main"))
    .way(&[4, 0, 3], named("Cross"))
}

/// Four arms meet a counter-clockwise one-way ring of radius 20 m.
fn roundabout() -> Net {
    let ring = Way {
        name: Some("Ring"),
        oneway: true,
        roundabout: true,
        ..Way::default()
    };
    Net::new(&[
        [0.0, -20.0],
        [20.0, 0.0],
        [0.0, 20.0],
        [-20.0, 0.0],
        [0.0, -200.0],
        [200.0, 0.0],
        [0.0, 200.0],
        [-200.0, 0.0],
    ])
    .way(&[0, 1, 2, 3, 0], ring)
    .way(&[4, 0], named("South Road"))
    .way(&[1, 5], named("East Road"))
    .way(&[2, 6], named("North Road"))
    .way(&[3, 7], named("West Road"))
}

#[test]
fn crossroads_turns_and_straight_through() {
    let net = crossroads(named("Main"));
    for (to, expected_kinds, instruction) in [
        (
            [100.0, 0.0],
            vec![ManeuverType::Right],
            "Turn right onto Cross.",
        ),
        (
            [-100.0, 0.0],
            vec![ManeuverType::Left],
            "Turn left onto Cross.",
        ),
        ([0.0, 100.0], vec![], ""),
    ] {
        let result = net.directions([0.0, -150.0], to);
        let mut expected = vec![ManeuverType::Depart];
        expected.extend(&expected_kinds);
        expected.push(ManeuverType::Arrive);
        assert_eq!(kinds(&result), expected, "to {to:?}");
        assert_eq!(result.maneuvers[0].instruction, "Head north on Main.");
        if !expected_kinds.is_empty() {
            assert_eq!(result.maneuvers[1].instruction, instruction);
            assert_eq!(result.maneuvers[1].street_name.as_deref(), Some("Cross"));
        }
        let total = result.maneuvers.iter().map(|m| m.distance_m).sum::<u64>();
        assert_eq!(total, result.route.summary.network_distance_m);
    }
}

#[test]
fn turn_angle_picks_slight_normal_and_sharp_turns() {
    use ManeuverType::*;
    for (degrees, expected) in [
        (30.0, SlightRight),
        (90.0, Right),
        (150.0, SharpRight),
        (-30.0, SlightLeft),
        (-90.0, Left),
        (-150.0, SharpLeft),
    ] {
        let radians = f64::to_radians(degrees);
        let end = [200.0 * radians.sin(), 200.0 * radians.cos()];
        let net = Net::new(&[[0.0, 0.0], [0.0, -200.0], [0.0, 200.0], end])
            .way(&[1, 0, 2], named("Main"))
            .way(&[0, 3], named("Side"));
        let result = net.directions([0.0, -150.0], [end[0] / 2.0, end[1] / 2.0]);
        assert_eq!(kinds(&result), [Depart, expected, Arrive], "{degrees}°");
    }
}

#[test]
fn bends_name_changes_and_shared_route_numbers() {
    use ManeuverType::*;
    let bend = [
        200.0 * 35_f64.to_radians().sin(),
        200.0 * 35_f64.to_radians().cos(),
    ];
    // Main bends 35° right at a junction with a side street: no instruction.
    let net = Net::new(&[[0.0, 0.0], [0.0, -200.0], bend, [-200.0, 0.0]])
        .way(&[1, 0, 2], named("Main"))
        .way(&[0, 3], named("Side"));
    let result = net.directions([0.0, -150.0], [bend[0] / 2.0, bend[1] / 2.0]);
    assert_eq!(kinds(&result), [Depart, Arrive]);

    // A new name straight ahead without a junction.
    let net = Net::new(&[[0.0, -200.0], [0.0, 0.0], [0.0, 200.0]])
        .way(&[0, 1], named("Main"))
        .way(&[1, 2], named("Second"));
    let result = net.directions([0.0, -150.0], [0.0, 100.0]);
    assert_eq!(kinds(&result), [Depart, Continue, Arrive]);
    assert_eq!(result.maneuvers[1].instruction, "Continue onto Second.");

    // A different name but the same route number is the same road.
    let route = |name| Way {
        name: Some(name),
        reference: Some("N355"),
        ..Way::default()
    };
    let net = Net::new(&[[0.0, -200.0], [0.0, 0.0], [0.0, 200.0], [200.0, 0.0]])
        .way(&[0, 1], route("Main"))
        .way(&[1, 2], route("Other"))
        .way(&[1, 3], named("Side"));
    let result = net.directions([0.0, -150.0], [0.0, 100.0]);
    assert_eq!(kinds(&result), [Depart, Arrive]);
    assert_eq!(
        result.maneuvers[0].instruction,
        "Head north on Main (N355)."
    );
}

#[test]
fn unnamed_unbranched_bend_needs_no_instruction() {
    let net =
        Net::new(&[[0.0, -200.0], [0.0, 0.0], [150.0, 120.0]]).way(&[0, 1, 2], Way::default());
    let result = net.directions([0.0, -150.0], [75.0, 60.0]);
    assert_eq!(kinds(&result), [ManeuverType::Depart, ManeuverType::Arrive]);
    assert_eq!(result.maneuvers[0].instruction, "Head north.");
}

#[test]
fn roundabout_exits_are_counted_along_the_ring() {
    let net = roundabout();
    for (to, count, street) in [
        ([100.0, 0.0], 1, "East Road"),
        ([0.0, 100.0], 2, "North Road"),
        ([-100.0, 0.0], 3, "West Road"),
    ] {
        let result = net.directions([0.0, -150.0], to);
        assert_eq!(
            kinds(&result),
            [
                ManeuverType::Depart,
                ManeuverType::EnterRoundabout,
                ManeuverType::ExitRoundabout,
                ManeuverType::Arrive
            ],
            "to {street}"
        );
        let enter = &result.maneuvers[1];
        assert_eq!(enter.roundabout_exit_count, Some(count));
        let ordinal = ["1st", "2nd", "3rd"][count as usize - 1];
        assert_eq!(
            enter.instruction,
            format!("Enter the roundabout and take the {ordinal} exit onto {street}.")
        );
        assert_eq!(
            result.maneuvers[2].instruction,
            format!("Exit the roundabout onto {street}.")
        );
    }
}

#[test]
fn ending_inside_a_roundabout_does_not_invent_an_exit() {
    let result = roundabout().directions([0.0, -150.0], [10.0, -10.0]);
    assert_eq!(result.maneuvers[1].kind, ManeuverType::EnterRoundabout);
    assert_eq!(result.maneuvers[1].roundabout_exit_count, None);
    assert_eq!(result.maneuvers[1].instruction, "Enter the roundabout.");
}

#[test]
fn roundabout_exit_count_excludes_a_restricted_exit() {
    // Edges 1 and 2 form the roundabout. Edge 4 leaves it at node 2 before
    // the route takes edge 3 at node 3, unless 0 -> 1 -> 4 is restricted.
    let nodes = (0..6)
        .map(|i| (6.0 + f64::from(i) * 0.001, 53.0))
        .collect::<Vec<_>>();
    let edges = [(0, 1), (1, 2), (2, 3), (3, 4), (2, 5)]
        .into_iter()
        .enumerate()
        .map(|(i, (from, to))| DirectedEdge {
            flags: if i == 1 || i == 2 {
                EDGE_FLAG_ROUNDABOUT
            } else {
                0
            },
            ..edge(i as u32, from, to, 10, 100)
        })
        .collect::<Vec<_>>();
    for restricted in [false, true] {
        let mut topology = topology("test", &nodes, &edges);
        topology.spatial_index = None;
        if restricted {
            topology = with_no_turn(topology, 1, &[0, 1, 4]);
        }
        let engine = exact_engine(topology, uniform_metrics(5, 10.0));
        let request = RouteRequest {
            snap: snap_within(1.0),
            ..route_request("directions", point("a", 6.0, 53.0), point("b", 6.004, 53.0))
        };
        let result = engine
            .execute_directions(
                &request,
                DirectionsContext::new(&EdgeNameBundle::default()),
                &DirectionsOptions::default(),
                EngineMode::Auto,
            )
            .unwrap();
        assert_eq!(result.route.edge_path, [0, 1, 2, 3]);
        assert_eq!(
            result.maneuvers[1].roundabout_exit_count,
            Some(if restricted { 1 } else { 2 })
        );
        assert_eq!(result.maneuvers[2].kind, ManeuverType::ExitRoundabout);
    }
}

/// A7 runs north; at point 1 a link leaves to the right (exit 36).
fn motorway_exit() -> Net {
    let motorway = Way {
        reference: Some("A7"),
        highway: Some(HighwayClass::Motorway),
        oneway: true,
        ..Way::default()
    };
    Net::new(&[
        [0.0, -1_500.0],
        [0.0, 0.0],
        [0.0, 1_500.0],
        [40.0, 300.0],
        [80.0, 700.0],
    ])
    .way(
        &[0, 1],
        Way {
            turn_lanes: Some("through|through;slight_right|slight_right"),
            ..motorway
        },
    )
    .way(&[1, 2], motorway)
    .way(
        &[1, 3, 4],
        Way {
            highway: Some(HighwayClass::MotorwayLink),
            oneway: true,
            destination: Some("Haren;Groningen-Zuid"),
            destination_ref: Some("N7"),
            junction_ref: Some("36"),
            ..Way::default()
        },
    )
}

#[test]
fn motorway_exit_carries_exit_number_signs_and_lanes() {
    let result = motorway_exit().directions([0.0, -1_000.0], [60.0, 500.0]);
    assert_eq!(
        kinds(&result),
        [
            ManeuverType::Depart,
            ManeuverType::ExitRight,
            ManeuverType::Arrive
        ]
    );
    let exit = &result.maneuvers[1];
    assert_eq!(
        exit.instruction,
        "Take exit 36 on the right toward N7, Haren, Groningen-Zuid."
    );
    assert_eq!(
        exit.signs,
        Some(Signs {
            exit_number: vec!["36".into()],
            branch: vec!["N7".into()],
            toward: vec!["Haren".into(), "Groningen-Zuid".into()],
            name: vec![],
        })
    );
    assert_eq!(
        exit.lanes
            .iter()
            .map(|lane| (lane.valid, lane.active))
            .collect::<Vec<_>>(),
        [(false, false), (true, true), (true, true)]
    );
    assert_eq!(
        exit.verbal.pre_alert.as_deref(),
        Some("In 1 kilometer, take exit 36 on the right toward N7, Haren, Groningen-Zuid.")
    );
    assert_eq!(exit.verbal.pre_alert_distance_m, Some(1_000));
    assert_eq!(result.maneuvers[0].instruction, "Head north on A7.");

    // Driving past the exit needs no instruction.
    let result = motorway_exit().directions([0.0, -1_000.0], [0.0, 1_000.0]);
    assert_eq!(kinds(&result), [ManeuverType::Depart, ManeuverType::Arrive]);
}

#[test]
fn link_roads_give_ramps_and_merges() {
    // Hereweg with an on-ramp to the right that merges onto the A28.
    let net = Net::new(&[
        [0.0, -300.0],
        [0.0, 0.0],
        [0.0, 300.0],
        [60.0, 100.0],
        [100.0, 300.0],
        [100.0, 1_300.0],
        [140.0, 300.0],
    ])
    .way(&[0, 1, 2], named("Hereweg"))
    .way(
        &[1, 3, 4],
        Way {
            highway: Some(HighwayClass::PrimaryLink),
            oneway: true,
            destination: Some("Assen"),
            ..Way::default()
        },
    )
    .way(
        &[6, 4, 5],
        Way {
            reference: Some("A28"),
            highway: Some(HighwayClass::Motorway),
            oneway: true,
            ..Way::default()
        },
    );
    let result = net.directions([0.0, -200.0], [100.0, 800.0]);
    assert_eq!(
        kinds(&result),
        [
            ManeuverType::Depart,
            ManeuverType::RampRight,
            ManeuverType::MergeLeft,
            ManeuverType::Arrive
        ]
    );
    assert_eq!(
        instructions(&result)[1..3],
        [
            "Take the ramp on the right toward Assen.",
            "Merge onto A28."
        ]
    );
}

#[test]
fn motorway_forks_keep_left_or_right_toward_signposted_destinations() {
    let motorway = |reference| Way {
        reference: Some(reference),
        highway: Some(HighwayClass::Motorway),
        oneway: true,
        ..Way::default()
    };
    let net = Net::new(&[
        [0.0, -1_000.0],
        [0.0, 0.0],
        [-150.0, 1_000.0],
        [150.0, 1_000.0],
    ])
    .way(&[0, 1], motorway("A7"))
    .way(
        &[1, 2],
        Way {
            destination: Some("Drachten"),
            ..motorway("A7")
        },
    )
    .way(
        &[1, 3],
        Way {
            destination: Some("Zwolle"),
            destination_ref: Some("A28"),
            ..motorway("A28")
        },
    );
    for (to, kind, instruction) in [
        (
            [75.0, 500.0],
            ManeuverType::KeepRight,
            "Keep right toward A28, Zwolle.",
        ),
        (
            [-75.0, 500.0],
            ManeuverType::KeepLeft,
            "Keep left toward Drachten.",
        ),
    ] {
        let result = net.directions([0.0, -500.0], to);
        assert_eq!(result.maneuvers[1].kind, kind);
        assert_eq!(result.maneuvers[1].instruction, instruction);
    }
}

/// A divided boulevard: northbound at x = 10, southbound at x = -10, and a
/// one-way crossover westward through the median at y = 0.
fn boulevard() -> Net {
    let carriageway = Way {
        name: Some("Boulevard"),
        oneway: true,
        ..Way::default()
    };
    Net::new(&[
        [10.0, -300.0],
        [10.0, 0.0],
        [10.0, 300.0],
        [-10.0, 300.0],
        [-10.0, 0.0],
        [-10.0, -300.0],
    ])
    .way(&[0, 1, 2], carriageway)
    .way(&[3, 4, 5], carriageway)
    .way(
        &[1, 4],
        Way {
            oneway: true,
            ..Way::default()
        },
    )
}

#[test]
fn median_crossover_collapses_into_a_u_turn_on_the_driving_side() {
    let result = boulevard().directions([10.0, -200.0], [-10.0, -200.0]);
    assert_eq!(
        kinds(&result),
        [
            ManeuverType::Depart,
            ManeuverType::UturnLeft,
            ManeuverType::Arrive
        ]
    );
    assert_eq!(
        result.maneuvers[1].instruction,
        "Make a U-turn onto Boulevard."
    );
    assert_eq!(result.driving_side, DrivingSide::Right);
    assert_eq!(result.driving_side_origin, DrivingSideOrigin::Default);

    struct LeftHandTraffic;
    impl DrivingSideSource for LeftHandTraffic {
        fn driving_side_at(&self, _lon: f64, _lat: f64) -> Option<DrivingSide> {
            Some(DrivingSide::Left)
        }
    }
    let net = boulevard();
    let names = net.names();
    let result = net
        .engine()
        .execute_directions(
            &net.request([10.0, -200.0], [-10.0, -200.0]),
            DirectionsContext {
                names: &names,
                driving_side_source: Some(&LeftHandTraffic),
            },
            &DirectionsOptions::default(),
            EngineMode::Auto,
        )
        .unwrap();
    assert_eq!(result.maneuvers[1].kind, ManeuverType::UturnRight);
    assert_eq!(result.driving_side_origin, DrivingSideOrigin::Dataset);
    // Without a source, the imported admin areas give the country's side.
    let mut topology = net.topology();
    topology.regions.admin_areas = vec![netweevil_core::AdminArea {
        country_code: "GB".to_string(),
        ..Default::default()
    }];
    topology.regions.node_admin_area = vec![0; topology.nodes.len()];
    let costs = net
        .edges
        .iter()
        .map(|edge| f64::from(edge.length_m) / 10.0)
        .collect::<Vec<_>>();
    let result = exact_engine(topology, metrics(&costs))
        .execute_directions(
            &net.request([10.0, -200.0], [-10.0, -200.0]),
            DirectionsContext::new(&names),
            &DirectionsOptions::default(),
            EngineMode::Auto,
        )
        .unwrap();
    assert_eq!(result.driving_side, DrivingSide::Left);
    assert_eq!(result.driving_side_origin, DrivingSideOrigin::Dataset);
    // The request overrides the dataset.
    let options = DirectionsOptions {
        driving_side: Some(DrivingSide::Right),
        ..DirectionsOptions::default()
    };
    let result = net.directions_with([10.0, -200.0], [-10.0, -200.0], &options);
    assert_eq!(result.driving_side_origin, DrivingSideOrigin::Request);
}

#[test]
fn urban_turn_lanes_mark_valid_lanes_for_the_turn() {
    let net = crossroads(Way {
        name: Some("Main"),
        turn_lanes: Some("left|through|right"),
        ..Way::default()
    });
    for (to, valid) in [([100.0, 0.0], "..x"), ([-100.0, 0.0], "x..")] {
        let result = net.directions([0.0, -150.0], to);
        let lanes = result.maneuvers[1]
            .lanes
            .iter()
            .map(|lane| if lane.valid { 'x' } else { '.' })
            .collect::<String>();
        assert_eq!(lanes, valid, "to {to:?}");
    }
    let straight = net.directions([0.0, -150.0], [0.0, 100.0]);
    assert!(
        straight
            .maneuvers
            .iter()
            .all(|maneuver| maneuver.lanes.is_empty())
    );
}

#[test]
fn arrival_side_follows_the_destination_offset() {
    let net = crossroads(named("Main"));
    for (to, side) in [
        ([100.0, 15.0], Some(Side::Left)),
        ([100.0, -15.0], Some(Side::Right)),
        ([100.0, 0.0], None),
    ] {
        let result = net.directions([0.0, -150.0], to);
        let arrive = result.maneuvers.last().unwrap();
        assert_eq!(arrive.arrival_side, side, "to {to:?}");
        let expected = match side {
            Some(Side::Left) => "Your destination is on the left.",
            Some(Side::Right) => "Your destination is on the right.",
            None => "You have arrived at your destination.",
        };
        assert_eq!(arrive.instruction, expected);
    }
}

#[test]
fn stairs_escalators_and_ferries_are_their_own_steps() {
    // Footway, a 6 m staircase up, footway.
    let net = Net::new(&[[0.0, -100.0], [0.0, 0.0], [0.0, 20.0], [0.0, 120.0]])
        .with_z(2, 6.0)
        .with_z(3, 6.0)
        .way(&[0, 1], Way::default())
        .way(
            &[1, 2],
            Way {
                highway: Some(HighwayClass::Steps),
                ..Way::default()
            },
        )
        .way(&[2, 3], Way::default());
    let result = net.directions([0.0, -50.0], [0.0, 80.0]);
    assert_eq!(
        kinds(&result),
        [
            ManeuverType::Depart,
            ManeuverType::Stairs,
            ManeuverType::Continue,
            ManeuverType::Arrive
        ]
    );
    assert_eq!(result.maneuvers[1].instruction, "Take the stairs up.");
    assert_eq!(result.maneuvers[1].level_change_m, Some(6.0));

    // An escalator identified by a coded feature attribute (8 = escalator).
    let mut net = Net::new(&[[0.0, -100.0], [0.0, 0.0], [0.0, 20.0], [0.0, 120.0]])
        .with_z(2, -6.0)
        .with_z(3, -6.0)
        .way(&[0, 1], Way::default())
        .way(
            &[1, 2],
            Way {
                feature_row: Some(0),
                ..Way::default()
            },
        )
        .way(&[2, 3], Way::default());
    net.feature_attributes = FeatureAttributeTable {
        row_count: 1,
        strings: Vec::new(),
        columns: vec![FeatureAttributeColumn {
            definition: FeatureAttributeDefinition {
                name: "FeatureType".into(),
                semantic_role: Some("feature_type".into()),
                value_type: FeatureAttributeType::Integer,
                domain: BTreeMap::from([("8".into(), "escalator".into())]),
            },
            data: FeatureAttributeColumnData::Integer(vec![Some(8)]),
        }],
    };
    let result = net.directions_with([0.0, -50.0], [0.0, 80.0], &in_language("zh-HK"));
    assert_eq!(result.maneuvers[1].kind, ManeuverType::Escalator);
    assert_eq!(result.maneuvers[1].instruction, "乘搭扶手電梯下樓。");

    let net = Net::new(&[[0.0, -100.0], [0.0, 0.0], [0.0, 500.0], [0.0, 600.0]])
        .way(&[0, 1], named("Kade"))
        .way(
            &[1, 2],
            Way {
                name: Some("Veerpont Zoutkamp"),
                highway: Some(HighwayClass::Ferry),
                ..Way::default()
            },
        )
        .way(&[2, 3], named("Havenweg"));
    let result = net.directions([0.0, -50.0], [0.0, 550.0]);
    assert_eq!(
        instructions(&result),
        [
            "Head north on Kade.",
            "Take the ferry Veerpont Zoutkamp.",
            "Leave the ferry.",
            "You have arrived at your destination."
        ]
    );
}

#[test]
fn instructions_render_in_every_language() {
    let turn = crossroads(named("Main"));
    let ring = roundabout();
    for (language, right_turn, roundabout_exit, arrive) in [
        (
            "en",
            "Turn right onto Cross.",
            "Enter the roundabout and take the 2nd exit onto North Road.",
            "You have arrived at your destination.",
        ),
        (
            "nl-NL",
            "Sla rechtsaf naar Cross.",
            "Neem op de rotonde de 2e afslag naar North Road.",
            "U bent op uw bestemming.",
        ),
        (
            "de",
            "Rechts abbiegen auf Cross.",
            "Im Kreisverkehr die 2. Ausfahrt nehmen auf North Road.",
            "Sie haben Ihr Ziel erreicht.",
        ),
        (
            "fr",
            "Tournez à droite sur Cross.",
            "Au rond-point, prenez la 2e sortie sur North Road.",
            "Vous êtes arrivé à destination.",
        ),
        (
            "fi",
            "Käänny oikealle tielle Cross.",
            "Aja liikenneympyrään ja poistu 2. liittymästä tielle North Road.",
            "Olet perillä.",
        ),
        (
            "zh-HK",
            "右轉進入Cross。",
            "駛入迴旋處，然後從第2個出口離開，進入North Road。",
            "已到達目的地。",
        ),
    ] {
        let options = in_language(language);
        let result = turn.directions_with([0.0, -150.0], [100.0, 0.0], &options);
        assert!(result.warnings.is_empty(), "{language}");
        assert_eq!(result.maneuvers[1].instruction, right_turn, "{language}");
        assert_eq!(result.maneuvers[2].instruction, arrive, "{language}");
        let result = ring.directions_with([0.0, -150.0], [0.0, 100.0], &options);
        assert_eq!(
            result.maneuvers[1].instruction, roundabout_exit,
            "{language}"
        );
    }
    let result = turn.directions_with([0.0, -150.0], [100.0, 0.0], &in_language("sv"));
    assert_eq!(result.language, "en");
    assert_eq!(result.warnings.len(), 1);
}

#[test]
fn verbal_instructions_announce_ahead_chain_close_turns_and_follow_units() {
    let net = crossroads(named("Main"));
    for (units, language, pre_alert, post_transition) in [
        (
            DistanceUnits::Metric,
            "en",
            "In 200 meters, turn right onto Cross.",
            "Continue for 200 meters.",
        ),
        (
            DistanceUnits::Imperial,
            "en",
            "In 0.1 miles, turn right onto Cross.",
            "Continue for 0.1 miles.",
        ),
        (
            DistanceUnits::Metric,
            "de",
            "In 200 Metern rechts abbiegen auf Cross.",
            "Der Straße 200 Meter folgen.",
        ),
        (
            DistanceUnits::Metric,
            "fi",
            "200 metrin päästä käänny oikealle tielle Cross.",
            "Jatka 200 metriä.",
        ),
    ] {
        let options = DirectionsOptions {
            units,
            ..in_language(language)
        };
        let result = net.directions_with([0.0, -200.0], [200.0, 0.0], &options);
        let turn = &result.maneuvers[1];
        assert_eq!(
            turn.verbal.pre_alert.as_deref(),
            Some(pre_alert),
            "{language} {units:?}"
        );
        assert_eq!(turn.verbal.pre_alert_distance_m, Some(200));
        assert_eq!(turn.verbal.alert, turn.instruction);
        assert_eq!(
            turn.verbal.post_transition.as_deref(),
            Some(post_transition),
            "{language} {units:?}"
        );
    }

    // Two turns 40 m apart are announced together.
    let net = Net::new(&[
        [0.0, -300.0],
        [0.0, 0.0],
        [0.0, 300.0],
        [40.0, 0.0],
        [300.0, 0.0],
        [40.0, 300.0],
    ])
    .way(&[0, 1, 2], named("Main"))
    .way(&[1, 3, 4], named("Short"))
    .way(&[3, 5], named("Far"));
    let result = net.directions([0.0, -200.0], [40.0, 200.0]);
    assert_eq!(
        result.maneuvers[1].verbal.alert,
        "Turn right onto Short, then turn left onto Far."
    );
    assert_eq!(result.maneuvers[1].verbal.post_transition, None);
    assert_eq!(result.maneuvers[2].verbal.pre_alert, None);
}

#[test]
fn time_dependent_routes_get_directions_with_times() {
    let net = crossroads(named("Main"));
    let names = net.names();
    let mut request = net.request([0.0, -150.0], [100.0, 0.0]);
    request.temporal.departure_time = Some("2026-09-09T08:00:00Z".into());
    let result = net
        .engine()
        .execute_directions(
            &request,
            DirectionsContext::new(&names),
            &DirectionsOptions::default(),
            EngineMode::Auto,
        )
        .expect("temporal directions succeed");
    assert_eq!(
        kinds(&result),
        [
            ManeuverType::Depart,
            ManeuverType::Right,
            ManeuverType::Arrive
        ]
    );
    assert!(
        result
            .maneuvers
            .iter()
            .all(|maneuver| maneuver.begin_time.is_some())
    );
}

#[test]
fn directions_reject_a_names_bundle_from_another_dataset() {
    let net = crossroads(named("Main"));
    let mut names = net.names();
    names.edge_guidance.pop();
    let error = net
        .engine()
        .execute_directions(
            &net.request([0.0, -150.0], [100.0, 0.0]),
            DirectionsContext::new(&names),
            &DirectionsOptions::default(),
            EngineMode::Auto,
        )
        .unwrap_err();
    assert!(error.to_string().contains("re-import"));
}

#[test]
fn a_short_jog_back_onto_the_same_street_needs_no_instruction() {
    let net = Net::new(&[
        [0.0, -200.0],
        [0.0, 0.0],
        [5.0, 0.0],
        [5.0, 200.0],
        [0.0, 200.0],
        [200.0, 0.0],
    ])
    .way(&[0, 1], named("Main"))
    .way(&[1, 2], Way::default())
    .way(&[2, 3], named("Main"))
    .way(&[1, 4], named("Other"))
    .way(&[2, 5], named("Side"));
    let result = net.directions([0.0, -150.0], [5.0, 100.0]);
    assert_eq!(kinds(&result), [ManeuverType::Depart, ManeuverType::Arrive]);
}

#[test]
fn walking_routes_get_no_ramps_or_lanes() {
    let net = Net::new(&[
        [0.0, -300.0],
        [0.0, 0.0],
        [0.0, 300.0],
        [60.0, 100.0],
        [100.0, 300.0],
    ])
    .way(&[0, 1, 2], named("Hereweg"))
    .way(
        &[1, 3, 4],
        Way {
            highway: Some(HighwayClass::PrimaryLink),
            turn_lanes: Some("through|slight_right"),
            ..Way::default()
        },
    );
    let names = net.names();
    let costs = net
        .edges
        .iter()
        .map(|edge| f64::from(edge.length_m))
        .collect::<Vec<_>>();
    let mut walking = metrics(&costs);
    walking.mode = netweevil_core::TravelMode::Foot;
    // Snapping only considers edges the profile's mode may use.
    let mut topology = net.topology();
    for profile in &mut topology.edge_layers.profile {
        profile.access_mask = netweevil_core::AccessMask::new(
            netweevil_core::AccessMask::CAR | netweevil_core::AccessMask::FOOT,
        );
    }
    let result = exact_engine(topology, walking)
        .execute_directions(
            &net.request([0.0, -200.0], [80.0, 200.0]),
            DirectionsContext::new(&names),
            &DirectionsOptions::default(),
            EngineMode::Auto,
        )
        .unwrap();
    assert_eq!(
        kinds(&result),
        [
            ManeuverType::Depart,
            ManeuverType::KeepRight,
            ManeuverType::Arrive
        ]
    );
    assert!(
        result
            .maneuvers
            .iter()
            .all(|maneuver| maneuver.lanes.is_empty())
    );
}
