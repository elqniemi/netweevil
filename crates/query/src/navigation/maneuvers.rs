//! Maneuver detection on a routed edge sequence.
//!
//! Every route edge after the first enters through a junction node. At
//! each junction the builder compares the route's heading with the other
//! legal branches (edge-based transitions allowed by the profile and the
//! turn-restriction automaton) and decides whether the driver needs an
//! instruction. Roundabouts count the legal exits passed; link roads give
//! ramps, exits and merges; near-parallel branches give forks. A cleanup
//! pass collapses the two short turns of a dual-carriageway crossing.

use netweevil_core::{
    EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, EdgeGuidance, EdgeNameBundle,
    HighwayClass, NodeId, RoadClass, TopologyBundle,
};

use super::lanes::{Lane, LaneMovement, lane_guidance, parse_turn_lanes};
use super::{ManeuverType, Side, Signs};
use crate::{DrivingSide, RouteResult, RoutingGraph};

/// Distance along the route used to measure headings at a junction, so a
/// short kink right at the node does not decide the turn.
const HEADING_LOOKAHEAD_M: u32 = 20;
/// Turns closer together than this across a junction pair are one turn
/// (crossing a dual carriageway).
const INTERNAL_EDGE_MAX_M: u64 = 30;
/// Turns joined by a connector this short are one turn (cycle path jogs,
/// a name change right after a turn).
const SHORT_CONNECTOR_M: u64 = 10;
/// How far back from a junction to look for `turn:lanes`.
const LANE_LOOKBACK_M: u32 = 250;
/// The next maneuver narrows the active lanes when it follows within this.
const ACTIVE_LANE_LOOKAHEAD_M: u64 = 300;
/// How far into a maneuver to look for signpost tags.
const SIGN_LOOKAHEAD_M: u32 = 200;
/// Destinations closer to the road than this get no arrival side.
const ARRIVAL_SIDE_MIN_OFFSET_M: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Facility {
    Stairs,
    Escalator,
    Elevator,
    Travelator,
}

impl Facility {
    const fn kind(self) -> ManeuverType {
        match self {
            Self::Stairs => ManeuverType::Stairs,
            Self::Escalator => ManeuverType::Escalator,
            Self::Elevator => ManeuverType::Elevator,
            Self::Travelator => ManeuverType::Travelator,
        }
    }

    /// Attribute values (raw or coded-domain labels) naming the facility.
    const fn values(self) -> &'static [&'static str] {
        match self {
            Self::Stairs => &["stairs", "steps", "stair", "staircase", "stairlift"],
            Self::Escalator => &["escalator"],
            Self::Elevator => &["lift", "elevator"],
            Self::Travelator => &["travelator", "moving_walkway"],
        }
    }
}

/// Name and route number of a street. Two streets are the same road when
/// they share the name or any route number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Street {
    pub(super) name: Option<String>,
    pub(super) reference: Option<String>,
}

impl Street {
    pub(super) fn is_empty(&self) -> bool {
        self.name.is_none() && self.reference.is_none()
    }

    fn refs(&self) -> impl Iterator<Item = &str> {
        self.reference
            .as_deref()
            .into_iter()
            .flat_map(|value| value.split(['/', ';']))
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub(super) fn same_road(&self, other: &Street) -> bool {
        let names =
            matches!((&self.name, &other.name), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b));
        names
            || self.refs().any(|reference| {
                other
                    .refs()
                    .any(|other| other.eq_ignore_ascii_case(reference))
            })
    }

    /// Display label: `Name (Ref)`, the name, or the route number.
    pub(super) fn label(&self) -> Option<String> {
        match (&self.name, &self.reference) {
            (Some(name), Some(reference)) => Some(format!("{name} ({reference})")),
            (Some(name), None) => Some(name.clone()),
            (None, Some(reference)) => Some(reference.clone()),
            (None, None) => None,
        }
    }
}

/// A maneuver before rendering.
#[derive(Debug, Clone)]
pub(super) struct Draft {
    pub(super) kind: ManeuverType,
    /// Half-open range of route positions.
    pub(super) begin: usize,
    pub(super) end: usize,
    pub(super) location: [f64; 2],
    pub(super) bearing_before: Option<f64>,
    pub(super) bearing_after: Option<f64>,
    /// Signed turn angle, positive to the right.
    pub(super) turn_delta: Option<f64>,
    pub(super) street: Street,
    /// Roundabout entry: exits passed and the street after the exit.
    pub(super) roundabout_exit_count: Option<u32>,
    pub(super) exit_street: Street,
    pub(super) arrival_side: Option<Side>,
    pub(super) level_change_m: Option<f64>,
    pub(super) signs: Option<Signs>,
    pub(super) lane_movement: Option<LaneMovement>,
    pub(super) lanes: Vec<Lane>,
    /// Road class of the edge leading into the maneuver.
    pub(super) approach_highway: Option<HighwayClass>,
}

impl Draft {
    fn new(kind: ManeuverType, position: usize, location: [f64; 2]) -> Self {
        Self {
            kind,
            begin: position,
            end: position,
            location,
            bearing_before: None,
            bearing_after: None,
            turn_delta: None,
            street: Street::default(),
            roundabout_exit_count: None,
            exit_street: Street::default(),
            arrival_side: None,
            level_change_m: None,
            signs: None,
            lane_movement: None,
            lanes: Vec::new(),
            approach_highway: None,
        }
    }
}

/// One other legal branch at a junction.
struct Branch {
    edge: usize,
    angle: f64,
}

struct Junction {
    in_heading: f64,
    out_heading: f64,
    /// Route turn angle, positive to the right.
    delta: f64,
    /// Legal branches other than the route's and the way back.
    others: Vec<Branch>,
    /// The route turns back onto the edge it came from.
    reverse: bool,
    /// The junction offers an exit off a roundabout.
    roundabout_exit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Straight,
    Slight,
    Normal,
    Sharp,
    Reverse,
}

fn turn_of(delta: f64) -> Turn {
    match delta.abs() {
        a if a < 15.0 => Turn::Straight,
        a if a < 45.0 => Turn::Slight,
        a if a < 135.0 => Turn::Normal,
        a if a < 160.0 => Turn::Sharp,
        _ => Turn::Reverse,
    }
}

fn normalize(angle: f64) -> f64 {
    let wrapped = (angle + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

const fn is_link(highway: HighwayClass) -> bool {
    matches!(
        highway,
        HighwayClass::MotorwayLink
            | HighwayClass::TrunkLink
            | HighwayClass::PrimaryLink
            | HighwayClass::SecondaryLink
            | HighwayClass::TertiaryLink
    )
}

const fn is_major(highway: HighwayClass) -> bool {
    matches!(highway, HighwayClass::Motorway | HighwayClass::Trunk)
}

/// Turn maneuver and lane movement for a signed angle.
fn turn_maneuver(delta: f64, driving_side: DrivingSide) -> (ManeuverType, LaneMovement) {
    let right = delta > 0.0;
    match (turn_of(delta), right) {
        (Turn::Straight, _) => (ManeuverType::Continue, LaneMovement::Through),
        (Turn::Slight, false) => (ManeuverType::SlightLeft, LaneMovement::SlightLeft),
        (Turn::Slight, true) => (ManeuverType::SlightRight, LaneMovement::SlightRight),
        (Turn::Normal, false) => (ManeuverType::Left, LaneMovement::Left),
        (Turn::Normal, true) => (ManeuverType::Right, LaneMovement::Right),
        (Turn::Sharp, false) => (ManeuverType::SharpLeft, LaneMovement::SharpLeft),
        (Turn::Sharp, true) => (ManeuverType::SharpRight, LaneMovement::SharpRight),
        (Turn::Reverse, _) => uturn(driving_side),
    }
}

/// U-turns go across oncoming traffic: left in right-hand traffic.
fn uturn(driving_side: DrivingSide) -> (ManeuverType, LaneMovement) {
    match driving_side {
        DrivingSide::Right => (
            ManeuverType::UturnLeft,
            LaneMovement::Reverse { left: true },
        ),
        DrivingSide::Left => (
            ManeuverType::UturnRight,
            LaneMovement::Reverse { left: false },
        ),
    }
}

fn is_plain_turn(kind: ManeuverType) -> bool {
    matches!(
        kind,
        ManeuverType::SlightLeft
            | ManeuverType::Left
            | ManeuverType::SharpLeft
            | ManeuverType::SlightRight
            | ManeuverType::Right
            | ManeuverType::SharpRight
    )
}

/// Maneuvers that a short connector can join into one turn.
fn is_turn_like(kind: ManeuverType) -> bool {
    is_plain_turn(kind)
        || matches!(
            kind,
            ManeuverType::Continue
                | ManeuverType::KeepLeft
                | ManeuverType::KeepRight
                | ManeuverType::KeepStraight
        )
}

/// A step onto `street` names it when the street is known and differs from
/// the one the previous step was on.
fn announces_street(previous: &Draft, street: &Street) -> bool {
    !street.is_empty() && !street.same_road(&previous.street)
}

/// Where `delta` lies among `angles`: left of all, right of all, or between.
fn relative_side(delta: f64, angles: impl IntoIterator<Item = f64> + Clone) -> Option<bool> {
    let left_of_all = angles.clone().into_iter().all(|angle| angle > delta);
    let right_of_all = angles.into_iter().all(|angle| angle < delta);
    match (left_of_all, right_of_all) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// The point `distance_m` from `from` toward `to` on an edge of
/// `length_m`, or `to` when the edge is shorter.
fn along(from: [f64; 2], to: [f64; 2], distance_m: u32, length_m: u32) -> [f64; 2] {
    if length_m == 0 || distance_m >= length_m {
        return to;
    }
    let fraction = f64::from(distance_m) / f64::from(length_m);
    [
        from[0] + (to[0] - from[0]) * fraction,
        from[1] + (to[1] - from[1]) * fraction,
    ]
}

pub(super) fn bearing(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dl = (b[0] - a[0]).to_radians();
    let (lat_a, lat_b) = (a[1].to_radians(), b[1].to_radians());
    (dl.sin() * lat_b.cos())
        .atan2(lat_a.cos() * lat_b.sin() - lat_a.sin() * lat_b.cos() * dl.cos())
        .to_degrees()
        .rem_euclid(360.0)
}

pub(super) struct ManeuverBuilder<'a> {
    pub(super) topology: &'a TopologyBundle,
    pub(super) graph: &'a RoutingGraph,
    pub(super) names: &'a EdgeNameBundle,
    pub(super) route: &'a RouteResult,
    pub(super) requested_destination: [f64; 2],
    pub(super) driving_side: DrivingSide,
    pub(super) facility_attribute: &'a str,
    /// Ramps, exits, merges, dual-carriageway U-turns and lanes are motor
    /// vehicle concepts; walking and cycling routes get plain turns.
    pub(super) motor_vehicle: bool,
}

impl ManeuverBuilder<'_> {
    fn edge(&self, position: usize) -> usize {
        self.route.edge_path[position] as usize
    }

    fn node(&self, node: NodeId) -> [f64; 2] {
        let node = &self.topology.nodes[node.0 as usize];
        [node.lon, node.lat]
    }

    fn highway(&self, edge: usize) -> HighwayClass {
        self.topology.edge_profile(edge).highway
    }

    fn roundabout(&self, edge: usize) -> bool {
        self.topology.routing_edge(edge).flags & EDGE_FLAG_ROUNDABOUT != 0
    }

    fn ferry(&self, edge: usize) -> bool {
        let profile = self.topology.edge_profile(edge);
        profile.road_class == RoadClass::Ferry || profile.highway == HighwayClass::Ferry
    }

    fn guidance(&self, edge: usize) -> EdgeGuidance {
        self.names.guidance(edge)
    }

    fn street(&self, position: usize) -> Street {
        let edge = self.edge(position);
        let segments = self.route.segments.as_deref().unwrap_or_default();
        let name = segments[position]
            .name
            .clone()
            .filter(|name| !name.trim().is_empty());
        let reference = self
            .names
            .string(self.guidance(edge).reference)
            .map(|value| {
                value
                    .split(';')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .filter(|value| !value.is_empty());
        Street { name, reference }
    }

    fn facility(&self, edge: usize, column: Option<usize>) -> Option<Facility> {
        let row = self.topology.routing_edge(edge).feature_row;
        let table = &self.topology.feature_attributes;
        let from_attribute = column.and_then(|column| {
            [
                Facility::Escalator,
                Facility::Travelator,
                Facility::Elevator,
                Facility::Stairs,
            ]
            .into_iter()
            .find(|facility| {
                facility
                    .values()
                    .iter()
                    .any(|value| table.value_matches_at(row, column, value))
            })
        });
        from_attribute
            .or_else(|| (self.highway(edge) == HighwayClass::Steps).then_some(Facility::Stairs))
    }

    /// Legal continuations of `prev` for the restriction state `state`,
    /// excluding the way back. Edges gated in the static graph (temporal
    /// direction overrides) fall back to the physical transitions.
    fn legal_branches(&self, prev: usize, state: usize, next_on_route: usize) -> Vec<usize> {
        let prev_edge = self.topology.routing_edge(prev);
        let static_range = self.graph.transition_range(prev);
        let mut branches = if static_range.is_empty() {
            let topology = &self.topology.edge_based_topology;
            topology.edge_transition_edges[topology.edge_transition_first_out[prev] as usize
                ..topology.edge_transition_first_out[prev + 1] as usize]
                .iter()
                .map(|&edge| edge as usize)
                .filter(|&edge| {
                    self.graph.edge_costs[edge].is_finite()
                        || self.topology.routing_edge(edge).flags
                            & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                            != 0
                })
                .collect::<Vec<_>>()
        } else {
            static_range
                .map(|transition| self.graph.transition_edges[transition] as usize)
                .collect::<Vec<_>>()
        };
        branches.retain(|&edge| {
            let candidate = self.topology.routing_edge(edge);
            edge == next_on_route
                || (candidate.to != prev_edge.from
                    && self.graph.automaton.is_transition_allowed(state, edge))
        });
        if !branches.contains(&next_on_route) {
            branches.push(next_on_route);
        }
        branches
    }

    /// Heading from the head of `edge` back toward the route start, measured
    /// over up to [`HEADING_LOOKAHEAD_M`]. The measuring point is
    /// interpolated on the edge that crosses the lookahead, so a long edge
    /// beyond a short connector does not stretch the heading past it.
    fn heading_into(&self, position: usize) -> f64 {
        let junction = self.node(self.topology.routing_edge(self.edge(position)).from);
        let mut travelled = 0;
        let mut point = junction;
        for back in (0..position).rev() {
            let edge = self.topology.routing_edge(self.edge(back));
            let (near, far) = (self.node(edge.to), self.node(edge.from));
            point = along(near, far, HEADING_LOOKAHEAD_M - travelled, edge.length_m);
            travelled += edge.length_m;
            if travelled >= HEADING_LOOKAHEAD_M {
                break;
            }
        }
        bearing(point, junction)
    }

    /// Heading from the junction at the start of `position` along the route.
    fn heading_out_of(&self, position: usize) -> f64 {
        let junction = self.node(self.topology.routing_edge(self.edge(position)).from);
        let mut travelled = 0;
        let mut point = junction;
        for ahead in position..self.route.edge_path.len() {
            let edge = self.topology.routing_edge(self.edge(ahead));
            let (near, far) = (self.node(edge.from), self.node(edge.to));
            point = along(near, far, HEADING_LOOKAHEAD_M - travelled, edge.length_m);
            travelled += edge.length_m;
            if travelled >= HEADING_LOOKAHEAD_M {
                break;
            }
        }
        bearing(junction, point)
    }

    /// Heading of a branch, following it while it has a single onward edge.
    fn heading_of_branch(&self, first: usize) -> f64 {
        let topology = &self.topology.edge_based_topology;
        let start = self.topology.routing_edge(first);
        let junction = self.node(start.from);
        let (mut edge_index, mut edge) = (first, start);
        let mut travelled = edge.length_m;
        for _ in 0..8 {
            if travelled >= HEADING_LOOKAHEAD_M {
                break;
            }
            let mut onward = topology.edge_transition_edges[topology.edge_transition_first_out
                [edge_index] as usize
                ..topology.edge_transition_first_out[edge_index + 1] as usize]
                .iter()
                .map(|&next| next as usize)
                .filter(|&next| {
                    self.graph.edge_costs[next].is_finite()
                        && self.topology.routing_edge(next).to != edge.from
                });
            let (Some(next), None) = (onward.next(), onward.next()) else {
                break;
            };
            edge_index = next;
            edge = self.topology.routing_edge(next);
            travelled += edge.length_m;
        }
        let before = travelled.saturating_sub(edge.length_m);
        let end = along(
            self.node(edge.from),
            self.node(edge.to),
            HEADING_LOOKAHEAD_M.saturating_sub(before),
            edge.length_m,
        );
        bearing(junction, end)
    }

    fn junction(&self, position: usize, state: usize) -> Junction {
        let prev = self.edge(position - 1);
        let next = self.edge(position);
        let prev_edge = self.topology.routing_edge(prev);
        let in_heading = self.heading_into(position);
        let out_heading = self.heading_out_of(position);
        let branches = self.legal_branches(prev, state, next);
        let roundabout_exit = branches.iter().any(|&edge| !self.roundabout(edge));
        let others = branches
            .into_iter()
            .filter(|&edge| edge != next)
            .map(|edge| Branch {
                edge,
                angle: normalize(self.heading_of_branch(edge) - in_heading),
            })
            .collect();
        Junction {
            in_heading,
            out_heading,
            delta: normalize(out_heading - in_heading),
            others,
            reverse: self.topology.routing_edge(next).to == prev_edge.from
                && self.topology.routing_edge(next).from == prev_edge.to,
            roundabout_exit,
        }
    }

    /// Decides the maneuver at an ordinary junction. `street_change` says
    /// the route moves onto a different named road.
    fn classify(
        &self,
        position: usize,
        junction: &Junction,
        street_change: bool,
        same_street: bool,
    ) -> Option<(ManeuverType, Option<LaneMovement>)> {
        let prev_highway = self.highway(self.edge(position - 1));
        let highway = self.highway(self.edge(position));
        let link = |highway| self.motor_vehicle && is_link(highway);
        let major = |highway| self.motor_vehicle && is_major(highway);
        let delta = junction.delta;
        let turn = turn_of(delta);
        if junction.reverse || turn == Turn::Reverse {
            let (kind, movement) = uturn(self.driving_side);
            return Some((kind, Some(movement)));
        }
        let other_angles = || junction.others.iter().map(|branch| branch.angle);
        let straightest = other_angles().all(|angle| angle.abs() >= delta.abs());
        if !link(prev_highway) && link(highway) {
            let left =
                relative_side(delta, other_angles().collect::<Vec<_>>()).unwrap_or(delta < 0.0);
            if major(prev_highway) {
                return Some(if left {
                    (ManeuverType::ExitLeft, Some(LaneMovement::KeepLeft))
                } else {
                    (ManeuverType::ExitRight, Some(LaneMovement::KeepRight))
                });
            }
            if turn == Turn::Straight && straightest {
                return Some((ManeuverType::RampStraight, Some(LaneMovement::Through)));
            }
            let (_, movement) = turn_maneuver(delta, self.driving_side);
            return Some(if left {
                (ManeuverType::RampLeft, Some(movement))
            } else {
                (ManeuverType::RampRight, Some(movement))
            });
        }
        // A link joining its own road class (or a motorway) at a shallow
        // angle merges; one ending at a crossroads turns.
        if link(prev_highway)
            && !link(highway)
            && (major(highway) || prev_highway.road_class() == highway.road_class())
            && delta.abs() < 60.0
        {
            let kind = match delta {
                d if d < -10.0 => ManeuverType::MergeLeft,
                d if d > 10.0 => ManeuverType::MergeRight,
                _ => ManeuverType::Merge,
            };
            return Some((kind, None));
        }
        let near = junction
            .others
            .iter()
            .filter(|branch| (branch.angle - delta).abs() < 40.0 && branch.angle.abs() < 60.0)
            .collect::<Vec<_>>();
        if delta.abs() < 60.0 && !near.is_empty() {
            let passing_exits =
                !link(highway) && near.iter().all(|branch| link(self.highway(branch.edge)));
            let major_fork = major(prev_highway) || link(prev_highway);
            if !passing_exits && (major_fork || !(same_street && straightest)) {
                let kind = match relative_side(delta, near.iter().map(|branch| branch.angle)) {
                    Some(true) => (ManeuverType::KeepLeft, LaneMovement::KeepLeft),
                    Some(false) => (ManeuverType::KeepRight, LaneMovement::KeepRight),
                    None => (ManeuverType::KeepStraight, LaneMovement::Through),
                };
                // On ordinary roads a slight turn off the road that carries
                // on is a turn, not a fork.
                if major_fork || turn == Turn::Straight || !street_change {
                    return Some((kind.0, Some(kind.1)));
                }
            }
            if passing_exits && same_street {
                return street_change.then_some((ManeuverType::Continue, None));
            }
        }
        let (kind, movement) = turn_maneuver(delta, self.driving_side);
        match turn {
            Turn::Straight => {
                street_change.then_some((ManeuverType::Continue, Some(LaneMovement::Through)))
            }
            _ if junction.others.is_empty() => {
                if !street_change {
                    None
                } else if turn == Turn::Slight {
                    Some((ManeuverType::Continue, None))
                } else {
                    Some((kind, Some(movement)))
                }
            }
            _ => {
                let clear_of_others = other_angles().all(|angle| (angle - delta).abs() >= 30.0);
                if same_street && straightest && clear_of_others && !street_change {
                    None
                } else {
                    Some((kind, Some(movement)))
                }
            }
        }
    }

    pub(super) fn build(&self) -> Vec<Draft> {
        let count = self.route.edge_path.len();
        let facility_column = self
            .topology
            .feature_attributes
            .column_index(self.facility_attribute);
        let facilities = (0..count)
            .map(|position| self.facility(self.edge(position), facility_column))
            .collect::<Vec<_>>();
        let mut drafts: Vec<Draft> = Vec::new();
        let mut state = 0;
        let mut roundabout_entry: Option<usize> = None;
        let mut exits = 0;
        let mut current_street = Street::default();
        let mut previous_street = Street::default();
        for position in 0..count {
            let edge = self.edge(position);
            let street = self.street(position);
            if position == 0 {
                let mut depart = Draft::new(
                    ManeuverType::Depart,
                    0,
                    [self.route.origin.snapped_lon, self.route.origin.snapped_lat],
                );
                depart.bearing_after = Some(self.heading_out_of(0));
                depart.street = street.clone();
                drafts.push(depart);
                // A route that starts on a facility departs with an empty
                // range and describes the facility as its first step.
                if let Some(facility) = facilities[0] {
                    let mut step = drafts[0].clone();
                    step.kind = facility.kind();
                    drafts.push(step);
                }
                current_street = street.clone();
                previous_street = street;
                drafts.last_mut().unwrap().end = 1;
                state = self.graph.automaton.transition(state, edge);
                continue;
            }
            let prev = self.edge(position - 1);
            let junction = self.junction(position, state);
            let location = self.node(self.topology.routing_edge(edge).from);
            let (was_roundabout, is_roundabout) = (self.roundabout(prev), self.roundabout(edge));
            if was_roundabout && junction.roundabout_exit {
                exits += 1;
            }
            let street_change = !street.is_empty() && !street.same_road(&current_street);
            let same_street = street.same_road(&previous_street)
                || (street.is_empty() && previous_street.is_empty());
            let decision: Option<(ManeuverType, Option<LaneMovement>)> = if !was_roundabout
                && is_roundabout
            {
                roundabout_entry = Some(drafts.len());
                exits = 0;
                Some((ManeuverType::EnterRoundabout, None))
            } else if was_roundabout && !is_roundabout {
                if let Some(entry) = roundabout_entry.take() {
                    let entry_draft = &mut drafts[entry];
                    entry_draft.roundabout_exit_count = Some(exits);
                    entry_draft.exit_street = street.clone();
                    let overall = normalize(
                        junction.out_heading
                            - entry_draft.bearing_before.unwrap_or(junction.in_heading),
                    );
                    entry_draft.lane_movement = Some(turn_maneuver(overall, self.driving_side).1);
                }
                Some((ManeuverType::ExitRoundabout, None))
            } else if is_roundabout {
                None
            } else if let Some(facility) = facilities[position]
                && (facilities[position - 1] != Some(facility)
                    || self.topology.routing_edge(prev).feature_row
                        != self.topology.routing_edge(edge).feature_row)
            {
                Some((facility.kind(), None))
            } else if self.ferry(prev) != self.ferry(edge) {
                Some(if self.ferry(edge) {
                    (ManeuverType::FerryEnter, None)
                } else {
                    (ManeuverType::FerryExit, None)
                })
            } else if facilities[position - 1].is_some() && facilities[position].is_none() {
                // Leaving a facility always gets an instruction so the
                // facility step covers only the facility.
                let (kind, movement) = turn_maneuver(junction.delta, self.driving_side);
                Some((kind, Some(movement)))
            } else {
                self.classify(position, &junction, street_change, same_street)
            };
            match decision {
                Some((kind, lane_movement)) => {
                    let mut draft = Draft::new(kind, position, location);
                    draft.bearing_before = Some(junction.in_heading);
                    draft.bearing_after = Some(junction.out_heading);
                    draft.turn_delta = Some(junction.delta);
                    draft.lane_movement = lane_movement;
                    draft.street = street.clone();
                    draft.approach_highway = Some(self.highway(prev));
                    drafts.push(draft);
                    current_street = street.clone();
                }
                None => {
                    let active = drafts.last_mut().expect("departure exists");
                    if current_street.is_empty() && !street.is_empty() {
                        current_street = street.clone();
                        if active.street.is_empty() {
                            active.street = street.clone();
                        }
                    }
                }
            }
            drafts.last_mut().expect("departure exists").end = position + 1;
            previous_street = street;
            state = self.graph.automaton.transition(state, edge);
        }
        self.collapse_short_turns(&mut drafts);
        self.finish_facilities(&mut drafts);
        self.attach_signs(&mut drafts);
        if self.motor_vehicle {
            self.attach_lanes(&mut drafts);
        }
        drafts.push(self.arrival(count));
        drafts
    }

    fn span_length(&self, draft: &Draft) -> u64 {
        let segments = self.route.segments.as_deref().unwrap_or_default();
        segments[draft.begin..draft.end]
            .iter()
            .map(|segment| u64::from(segment.length_m))
            .sum()
    }

    /// Joins turns separated by a very short connector into one turn, and
    /// for motor vehicles the two turns of a dual-carriageway crossing into
    /// one turn or a U-turn. Only a dual-carriageway crossing becomes a
    /// U-turn: two same-side turns around a connector (a cycle path hairpin)
    /// are one sharp turn. A short jog with little overall heading change
    /// needs no instruction unless it moves onto a differently named street.
    fn collapse_short_turns(&self, drafts: &mut Vec<Draft>) {
        let mut index = 1;
        while index < drafts.len() {
            if index + 1 < drafts.len() && self.merge_with_next(drafts, index) {
                continue;
            }
            if !self.motor_vehicle && self.settle_single_jog(drafts, index) {
                continue;
            }
            index += 1;
        }
    }

    /// Merges `drafts[index]` with the next draft when they are the two
    /// turns of a short connector or a crossing. Returns whether the drafts
    /// changed.
    fn merge_with_next(&self, drafts: &mut Vec<Draft>, index: usize) -> bool {
        let span = self.span_length(&drafts[index]);
        let (first, second) = (&drafts[index], &drafts[index + 1]);
        let crossing = self.motor_vehicle
            && is_plain_turn(first.kind)
            && is_plain_turn(second.kind)
            && span <= INTERNAL_EDGE_MAX_M
            && matches!(
                (first.turn_delta, second.turn_delta),
                (Some(a), Some(b)) if a.signum() == b.signum() && a.abs() >= 45.0 && b.abs() >= 45.0
            );
        let connector =
            is_turn_like(first.kind) && is_turn_like(second.kind) && span <= SHORT_CONNECTOR_M;
        if !(crossing || connector) {
            return false;
        }
        // The turn angles of two close junctions overlap (each heading looks
        // past the connector), so their sum overstates the turn. The overall
        // change is measured from the approach to the departure, wound to
        // the side the two turns go.
        let summed = first.turn_delta.unwrap_or(0.0) + second.turn_delta.unwrap_or(0.0);
        let rotation = match (first.bearing_before, second.bearing_after) {
            (Some(before), Some(after)) => summed + normalize(after - before - summed),
            _ => summed,
        };
        let (kind, movement) = match turn_of(rotation) {
            Turn::Reverse if crossing => uturn(self.driving_side),
            Turn::Reverse if rotation > 0.0 => (ManeuverType::SharpRight, LaneMovement::SharpRight),
            Turn::Reverse => (ManeuverType::SharpLeft, LaneMovement::SharpLeft),
            _ => turn_maneuver(rotation, self.driving_side),
        };
        let second = drafts.remove(index + 1);
        if kind == ManeuverType::Continue && !announces_street(&drafts[index - 1], &second.street) {
            let first = drafts.remove(index);
            drafts[index - 1].end = second.end.max(first.end);
            return true;
        }
        let first = &mut drafts[index];
        first.kind = kind;
        first.lane_movement = Some(movement);
        first.turn_delta = Some(rotation);
        first.end = second.end;
        first.bearing_after = second.bearing_after;
        if !second.street.is_empty() {
            first.street = second.street;
        }
        true
    }

    /// A walking or cycling step whose junction is followed within
    /// [`SHORT_CONNECTOR_M`] by another junction the route passes without
    /// an instruction: a cycle path jog when the heading after the second
    /// junction is about the heading before the first. The jog is dropped,
    /// or kept as a plain continue when it leads onto a newly named street.
    /// Returns whether the drafts changed.
    fn settle_single_jog(&self, drafts: &mut Vec<Draft>, index: usize) -> bool {
        let draft = &drafts[index];
        if !is_turn_like(draft.kind) || draft.kind == ManeuverType::Continue {
            return false;
        }
        let Some(before) = draft.bearing_before else {
            return false;
        };
        let mut connector_m = 0;
        let mut exit = None;
        for position in draft.begin..draft.end.saturating_sub(1) {
            connector_m += self.topology.routing_edge(self.edge(position)).length_m;
            if u64::from(connector_m) > SHORT_CONNECTOR_M {
                break;
            }
            exit = Some(position + 1);
        }
        let Some(exit) = exit else {
            return false;
        };
        let overall = normalize(self.heading_out_of(exit) - before);
        if turn_of(overall) != Turn::Straight {
            return false;
        }
        let street = self.street(exit);
        if announces_street(&drafts[index - 1], &street) {
            let draft = &mut drafts[index];
            draft.kind = ManeuverType::Continue;
            draft.lane_movement = Some(LaneMovement::Through);
            draft.turn_delta = Some(overall);
            draft.street = street;
        } else {
            let draft = drafts.remove(index);
            drafts[index - 1].end = draft.end;
        }
        true
    }

    fn finish_facilities(&self, drafts: &mut [Draft]) {
        for draft in drafts {
            if !matches!(
                draft.kind,
                ManeuverType::Stairs | ManeuverType::Escalator | ManeuverType::Elevator
            ) {
                continue;
            }
            let first = self.topology.routing_edge(self.edge(draft.begin));
            let last = self.topology.routing_edge(self.edge(draft.end - 1));
            let from = &self.topology.nodes[first.from.0 as usize];
            let to = &self.topology.nodes[last.to.0 as usize];
            draft.level_change_m = from
                .elevation_m()
                .zip(to.elevation_m())
                .map(|(from, to)| ((to - from) * 100.0).round() / 100.0);
        }
    }

    fn signs_of(&self, guidance: &EdgeGuidance) -> Signs {
        let split = |index: Option<u32>| -> Vec<String> {
            self.names
                .string(index)
                .into_iter()
                .flat_map(|value| value.split(';'))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        };
        let mut toward = split(guidance.destination);
        for extra in [guidance.destination_street, guidance.exit_to] {
            for value in split(extra) {
                if !toward.contains(&value) {
                    toward.push(value);
                }
            }
        }
        Signs {
            exit_number: split(guidance.junction_ref),
            branch: split(guidance.destination_ref),
            toward,
            name: split(guidance.junction_name),
        }
    }

    fn attach_signs(&self, drafts: &mut [Draft]) {
        for draft in drafts.iter_mut().skip(1) {
            if matches!(
                draft.kind,
                ManeuverType::ExitRoundabout
                    | ManeuverType::FerryEnter
                    | ManeuverType::FerryExit
                    | ManeuverType::Stairs
                    | ManeuverType::Escalator
                    | ManeuverType::Elevator
                    | ManeuverType::Travelator
            ) {
                continue;
            }
            let mut travelled = 0;
            for position in draft.begin..draft.end {
                let edge = self.edge(position);
                let signs = self.signs_of(&self.guidance(edge));
                if !signs.is_empty() {
                    draft.signs = Some(signs);
                    break;
                }
                travelled += self.topology.routing_edge(edge).length_m;
                if travelled >= SIGN_LOOKAHEAD_M {
                    break;
                }
            }
        }
    }

    /// `turn:lanes` of the approach to the junction at `position`: the
    /// nearest tagged edge before it that is not separated from the
    /// junction by another branching junction.
    fn approach_lanes(&self, position: usize) -> Option<String> {
        let mut travelled = 0;
        for back in (0..position).rev() {
            let edge = self.edge(back);
            if let Some(value) = self.names.string(self.guidance(edge).turn_lanes) {
                return Some(value.to_string());
            }
            travelled += self.topology.routing_edge(edge).length_m;
            if travelled >= LANE_LOOKBACK_M || back == 0 {
                return None;
            }
            // Stop at a junction with a real choice: its lanes belong to it.
            if self.choice_count(self.edge(back - 1)) > 1 {
                return None;
            }
        }
        None
    }

    /// Continuations of `edge` other than turning back.
    fn choice_count(&self, edge: usize) -> usize {
        let from = self.topology.routing_edge(edge).from;
        self.graph
            .transition_range(edge)
            .filter(|&transition| {
                self.topology
                    .routing_edge(self.graph.transition_edges[transition] as usize)
                    .to
                    != from
            })
            .count()
    }

    fn attach_lanes(&self, drafts: &mut [Draft]) {
        for index in 1..drafts.len() {
            let Some(movement) = drafts[index].lane_movement else {
                continue;
            };
            let Some(lanes) = self
                .approach_lanes(drafts[index].begin)
                .and_then(|value| parse_turn_lanes(&value))
            else {
                continue;
            };
            let next = drafts
                .get(index + 1)
                .filter(|_| self.span_length(&drafts[index]) <= ACTIVE_LANE_LOOKAHEAD_M)
                .and_then(|next| next.lane_movement);
            let guidance = lane_guidance(&lanes, movement, next);
            if guidance.iter().any(|lane| lane.valid) {
                drafts[index].lanes = guidance;
            }
        }
    }

    fn arrival(&self, count: usize) -> Draft {
        let destination = &self.route.destination;
        let snapped = [destination.snapped_lon, destination.snapped_lat];
        let mut arrive = Draft::new(ManeuverType::Arrive, count, snapped);
        arrive.approach_highway = Some(self.highway(self.edge(count - 1)));
        let last = self.topology.routing_edge(self.edge(count - 1));
        let (from, to) = (self.node(last.from), self.node(last.to));
        arrive.bearing_before = Some(bearing(from, to));
        let cos_lat = snapped[1].to_radians().cos();
        let along = [(to[0] - from[0]) * cos_lat, to[1] - from[1]];
        let offset = [
            (self.requested_destination[0] - snapped[0]) * cos_lat,
            self.requested_destination[1] - snapped[1],
        ];
        let offset_m = offset[0].hypot(offset[1]) * 111_320.0;
        let cross = along[0] * offset[1] - along[1] * offset[0];
        if offset_m >= ARRIVAL_SIDE_MIN_OFFSET_M && cross != 0.0 {
            arrive.arrival_side = Some(if cross > 0.0 { Side::Left } else { Side::Right });
        }
        arrive
    }
}
