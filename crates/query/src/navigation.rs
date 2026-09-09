//! English maneuver instructions for legal street routes.

use anyhow::{Result, ensure};
use netweevil_core::EDGE_FLAG_ROUNDABOUT;
use netweevil_profile::ReturnGeometry;
use serde::{Deserialize, Serialize};

use crate::{AnalysisOutcome, EngineMode, PreparedRoutingEngine, RouteRequest, RouteResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManeuverType {
    Depart,
    Continue,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    Uturn,
    EnterRoundabout,
    ExitRoundabout,
    Arrive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Maneuver {
    pub kind: ManeuverType,
    pub instruction: String,
    pub street_name: Option<String>,
    pub location: [f64; 2],
    /// Half-open range in `route.edge_path`. Arrival has an empty range.
    pub begin_edge_index: usize,
    pub end_edge_index: usize,
    pub bearing_before: Option<f64>,
    pub bearing_after: Option<f64>,
    /// Sum of the route's rounded edge lengths in this maneuver.
    pub distance_m: u64,
    /// Static edge travel time; turn penalties remain in route.summary.
    pub edge_time_s: f64,
    /// Legal exit junctions passed, including the selected exit. Absent when
    /// the route begins or ends inside the roundabout.
    pub roundabout_exit_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionsResult {
    pub language: String,
    pub route: RouteResult,
    pub maneuvers: Vec<Maneuver>,
}

impl PreparedRoutingEngine {
    pub fn execute_directions(
        &self,
        request: &RouteRequest,
        edge_names: &[String],
        mode: EngineMode,
    ) -> Result<DirectionsResult> {
        let mut request = request.clone();
        request.returns.segment_rows = true;
        request.returns.geometry = ReturnGeometry::Full;
        ensure!(
            !request.temporal.is_temporal(),
            "directions currently require a static street route"
        );
        ensure!(
            matches!(mode, EngineMode::Auto),
            "directions require all turn restrictions to be enforced"
        );
        let route = self.execute_route_with_edge_names_and_mode(&request, edge_names, mode)?;
        let maneuvers = self.route_maneuvers(&route, mode)?;
        Ok(DirectionsResult {
            language: "en".into(),
            route,
            maneuvers,
        })
    }

    pub(crate) fn route_maneuvers(
        &self,
        route: &RouteResult,
        mode: EngineMode,
    ) -> Result<Vec<Maneuver>> {
        ensure!(
            route.outcome == AnalysisOutcome::Legal
                && route.hop_segments.is_empty()
                && route.violations.is_empty(),
            "directions require a legal street route without network hops"
        );
        let segments = route.segments.as_deref().unwrap_or_default();
        ensure!(
            segments.len() == route.edge_path.len(),
            "directions require route segments"
        );
        let topology = self.topology();
        let (graph, _) = self.routing_graph_for_mode(mode);
        let mut result: Vec<Maneuver> = Vec::new();
        let mut state = 0;
        let mut roundabout_entry = None;
        let mut exits = 0;
        let mut last_bearing = None;
        for (i, segment) in segments.iter().enumerate() {
            let edge_index = route.edge_path[i] as usize;
            let edge = topology.routing_edge(edge_index);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            let after = bearing([from.lon, from.lat], [to.lon, to.lat]);
            last_bearing = Some(after);
            let mut before = None;
            let mut kind = None;
            if i == 0 {
                kind = Some(ManeuverType::Depart);
            } else {
                let prev_index = route.edge_path[i - 1] as usize;
                let prev = topology.routing_edge(prev_index);
                let prev_from = &topology.nodes[prev.from.0 as usize];
                before = Some(bearing(
                    [prev_from.lon, prev_from.lat],
                    [from.lon, from.lat],
                ));
                let was_roundabout = prev.flags & EDGE_FLAG_ROUNDABOUT != 0;
                let is_roundabout = edge.flags & EDGE_FLAG_ROUNDABOUT != 0;
                let legal_exits = graph
                    .transition_range(prev_index)
                    .map(|t| graph.transition_edges[t] as usize)
                    .filter(|&next| {
                        let candidate = topology.routing_edge(next);
                        candidate.to != prev.from
                            && graph.automaton.is_transition_allowed(state, next)
                    })
                    .collect::<Vec<_>>();
                if was_roundabout
                    && legal_exits
                        .iter()
                        .any(|&next| topology.routing_edge(next).flags & EDGE_FLAG_ROUNDABOUT == 0)
                {
                    exits += 1;
                }
                if !was_roundabout && is_roundabout {
                    kind = Some(ManeuverType::EnterRoundabout);
                    roundabout_entry = Some(result.len());
                    exits = 0;
                } else if was_roundabout && !is_roundabout {
                    kind = Some(ManeuverType::ExitRoundabout);
                    if let Some(entry) = roundabout_entry.take() {
                        result[entry].roundabout_exit_count = Some(exits);
                        result[entry].instruction =
                            format!("Enter the roundabout and take exit {exits}.");
                    }
                } else if !is_roundabout {
                    let turn = maneuver_turn(before.unwrap(), after);
                    let name_changed = segments[i - 1].name != segment.name;
                    if turn == ManeuverType::Uturn
                        || name_changed
                        || (legal_exits.len() > 1 && turn != ManeuverType::Continue)
                    {
                        kind = Some(turn);
                    }
                }
            }
            if let Some(kind) = kind {
                let location = if i == 0 {
                    [route.origin.snapped_lon, route.origin.snapped_lat]
                } else {
                    [from.lon, from.lat]
                };
                result.push(Maneuver {
                    kind,
                    instruction: instruction(kind, segment.name.as_deref()),
                    street_name: segment.name.clone(),
                    location,
                    begin_edge_index: i,
                    end_edge_index: i,
                    bearing_before: before,
                    bearing_after: Some(after),
                    distance_m: 0,
                    edge_time_s: 0.0,
                    roundabout_exit_count: None,
                });
            }
            let maneuver = result.last_mut().expect("departure maneuver exists");
            maneuver.end_edge_index = i + 1;
            maneuver.distance_m += u64::from(segment.length_m);
            maneuver.edge_time_s += segment.travel_time_s;
            state = graph.automaton.transition(state, edge_index);
        }
        result.push(Maneuver {
            kind: ManeuverType::Arrive,
            instruction: "You have arrived at your destination.".into(),
            street_name: None,
            location: [route.destination.snapped_lon, route.destination.snapped_lat],
            begin_edge_index: segments.len(),
            end_edge_index: segments.len(),
            bearing_before: last_bearing,
            bearing_after: None,
            distance_m: 0,
            edge_time_s: 0.0,
            roundabout_exit_count: None,
        });
        Ok(result)
    }
}

fn bearing(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dl = (b[0] - a[0]).to_radians();
    let (lat_a, lat_b) = (a[1].to_radians(), b[1].to_radians());
    (dl.sin() * lat_b.cos())
        .atan2(lat_a.cos() * lat_b.sin() - lat_a.sin() * lat_b.cos() * dl.cos())
        .to_degrees()
        .rem_euclid(360.0)
}

fn maneuver_turn(before: f64, after: f64) -> ManeuverType {
    let angle = (after - before + 180.0).rem_euclid(360.0) - 180.0;
    match (angle.abs(), angle > 0.0) {
        (a, _) if a < 15.0 => ManeuverType::Continue,
        (a, _) if a >= 165.0 => ManeuverType::Uturn,
        (a, false) if a < 45.0 => ManeuverType::SlightLeft,
        (a, true) if a < 45.0 => ManeuverType::SlightRight,
        (a, false) if a < 135.0 => ManeuverType::Left,
        (a, true) if a < 135.0 => ManeuverType::Right,
        (_, false) => ManeuverType::SharpLeft,
        (_, true) => ManeuverType::SharpRight,
    }
}

fn instruction(kind: ManeuverType, name: Option<&str>) -> String {
    let verb = match kind {
        ManeuverType::Depart => "Depart",
        ManeuverType::Continue => "Continue",
        ManeuverType::SlightLeft => "Bear left",
        ManeuverType::Left => "Turn left",
        ManeuverType::SharpLeft => "Turn sharp left",
        ManeuverType::SlightRight => "Bear right",
        ManeuverType::Right => "Turn right",
        ManeuverType::SharpRight => "Turn sharp right",
        ManeuverType::Uturn => "Make a U-turn",
        ManeuverType::EnterRoundabout => "Enter the roundabout",
        ManeuverType::ExitRoundabout => "Exit the roundabout",
        ManeuverType::Arrive => "Arrive",
    };
    match name.filter(|s| !s.trim().is_empty()) {
        Some(name) => format!("{verb} on {name}."),
        None => format!("{verb}."),
    }
}
