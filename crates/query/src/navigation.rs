//! Turn-by-turn directions for legal street routes, static or
//! time-dependent: maneuver detection (turns, forks, ramps, merges,
//! roundabouts, ferries, pedestrian facilities), lane guidance, signposts,
//! and localized written and spoken instructions.
//!
//! [`maneuvers`] turns the routed edge sequence into maneuvers using the
//! edge-based graph (legal branches, roundabout exits) and the edge-name
//! bundle (names, route numbers, lanes, signposts). [`narrative`] renders
//! them with the phrase tables in [`phrases`].

mod lanes;
mod maneuvers;
mod narrative;
mod phrases;
mod transit_steps;

use anyhow::{Result, ensure};
use netweevil_core::{EdgeNameBundle, TravelMode};
use netweevil_profile::{ProfileDocument, ReturnGeometry};
use serde::{Deserialize, Serialize};

use crate::{
    AnalysisOutcome, DrivingSide, EngineMode, PreparedRoutingEngine, RouteRequest, RouteResult,
};

pub use lanes::{Lane, LaneIndication};
pub use phrases::guidance_languages;
pub use transit_steps::{Phrasebook, StreetLegMode, TransitStep};

/// Pedestrian facility attribute read when the request names none; the
/// same default as profile `facilities.attribute`.
pub const DEFAULT_FACILITY_ATTRIBUTE: &str = "feature_type";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManeuverType {
    Depart,
    /// Straight on, or onto a differently named street.
    Continue,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    UturnLeft,
    UturnRight,
    /// Onto a link road from an ordinary road.
    RampLeft,
    RampRight,
    RampStraight,
    /// Off a motorway or trunk road onto a link road.
    ExitLeft,
    ExitRight,
    /// At a fork.
    KeepLeft,
    KeepRight,
    KeepStraight,
    /// From a link road onto a motorway or trunk road.
    Merge,
    MergeLeft,
    MergeRight,
    EnterRoundabout,
    ExitRoundabout,
    FerryEnter,
    FerryExit,
    Stairs,
    Escalator,
    Elevator,
    Travelator,
    Arrive,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistanceUnits {
    #[default]
    Metric,
    Imperial,
}

/// Where the driving side of a response came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrivingSideOrigin {
    Request,
    Dataset,
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

/// Request options for directions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionsOptions {
    /// BCP 47 language tag (`nl`, `de-CH`, `zh-HK`). Tags without a phrase
    /// table fall back to English with a warning.
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub units: DistanceUnits,
    /// Overrides the dataset's driving side, which otherwise comes from the
    /// [`DrivingSideSource`] (right-hand traffic when there is none).
    #[serde(default)]
    pub driving_side: Option<DrivingSide>,
    /// Feature attribute that classifies stairs, escalators, lifts and
    /// moving walkways. Defaults to [`DEFAULT_FACILITY_ATTRIBUTE`].
    #[serde(default)]
    pub facility_attribute: Option<String>,
}

impl DirectionsOptions {
    /// Reads pedestrian facilities from the profile's `facilities.attribute`
    /// unless the request names an attribute of its own.
    pub fn use_profile_facility_attribute(&mut self, profile: &ProfileDocument) {
        if self.facility_attribute.is_none() {
            self.facility_attribute = Some(profile.facilities.attribute.clone());
        }
    }
}

/// Dataset hook that knows which side of the road traffic keeps to at a
/// location (for example from admin boundaries).
pub trait DrivingSideSource: Send + Sync {
    fn driving_side_at(&self, lon: f64, lat: f64) -> Option<DrivingSide>;
}

/// Dataset context for directions beyond the routing engine itself.
#[derive(Clone, Copy)]
pub struct DirectionsContext<'a> {
    pub names: &'a EdgeNameBundle,
    pub driving_side_source: Option<&'a dyn DrivingSideSource>,
}

impl<'a> DirectionsContext<'a> {
    pub fn new(names: &'a EdgeNameBundle) -> Self {
        Self {
            names,
            driving_side_source: None,
        }
    }
}

/// Spoken forms of an instruction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerbalInstructions {
    /// Announced ahead of the maneuver ("In 300 meters, turn left").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_alert: Option<String>,
    /// Distance before the maneuver at which `pre_alert` is meant to play.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_alert_distance_m: Option<u32>,
    /// Announced at the maneuver; includes the next maneuver when it
    /// follows closely ("Turn left, then turn right").
    pub alert: String,
    /// Announced after the maneuver ("Continue for 2 kilometers").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_transition: Option<String>,
}

/// Signpost text at a maneuver, from OSM `junction:ref`, `destination:ref`,
/// `destination`, `destination:street`, `exit_to` and junction names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exit_number: Vec<String>,
    /// Route numbers the exit or branch leads to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branch: Vec<String>,
    /// Places and streets the exit or branch leads toward.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toward: Vec<String>,
    /// Name of the junction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name: Vec<String>,
}

impl Signs {
    pub fn is_empty(&self) -> bool {
        self.exit_number.is_empty()
            && self.branch.is_empty()
            && self.toward.is_empty()
            && self.name.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Maneuver {
    pub kind: ManeuverType,
    pub instruction: String,
    pub verbal: VerbalInstructions,
    /// Name of the street the maneuver leads onto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street_name: Option<String>,
    /// Route number of that street (`ref`, `;` separated values joined by `/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street_ref: Option<String>,
    pub location: [f64; 2],
    /// Half-open range in `route.edge_path`. Arrival has an empty range.
    pub begin_edge_index: usize,
    pub end_edge_index: usize,
    pub bearing_before: Option<f64>,
    pub bearing_after: Option<f64>,
    /// Sum of the route's rounded edge lengths in this maneuver.
    pub distance_m: u64,
    /// Edge travel time; turn penalties remain in route.summary.
    pub edge_time_s: f64,
    /// Time-dependent routes: waiting included in this maneuver.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waiting_time_s: f64,
    /// Time-dependent routes: when the maneuver begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_time: Option<String>,
    /// Legal exits passed, including the one taken. Absent when the route
    /// begins or ends inside the roundabout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roundabout_exit_count: Option<u32>,
    /// Arrival: side of the street the destination lies on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival_side: Option<Side>,
    /// Stairs, escalators and lifts: height change in metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_change_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signs: Option<Signs>,
    /// Lanes of the approach, left to right.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<Lane>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionsResult {
    /// Language of the instructions (a phrase table tag).
    pub language: String,
    pub units: DistanceUnits,
    pub driving_side: DrivingSide,
    pub driving_side_origin: DrivingSideOrigin,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub route: RouteResult,
    pub maneuvers: Vec<Maneuver>,
}

impl PreparedRoutingEngine {
    /// Routes `request` and describes the route. Time-dependent requests
    /// get the same maneuvers plus per-maneuver begin times and waits.
    pub fn execute_directions(
        &self,
        request: &RouteRequest,
        context: DirectionsContext<'_>,
        options: &DirectionsOptions,
        mode: EngineMode,
    ) -> Result<DirectionsResult> {
        ensure!(
            matches!(mode, EngineMode::Auto),
            "directions require all turn restrictions to be enforced"
        );
        ensure!(
            context.names.edge_guidance.is_empty()
                || context.names.edge_guidance.len() == self.topology().edge_count(),
            "the edge-name bundle does not belong to this topology; re-import the dataset"
        );
        let mut request = request.clone();
        request.returns.segment_rows = true;
        request.returns.geometry = ReturnGeometry::Full;
        let route =
            self.execute_route_with_edge_names_and_mode(&request, &context.names.names, mode)?;
        self.directions_for_route(route, &request, context, options, mode)
    }

    /// Describes an already computed route. `request` must be the request
    /// that produced it.
    pub fn directions_for_route(
        &self,
        route: RouteResult,
        request: &RouteRequest,
        context: DirectionsContext<'_>,
        options: &DirectionsOptions,
        mode: EngineMode,
    ) -> Result<DirectionsResult> {
        ensure!(
            route.outcome == AnalysisOutcome::Legal
                && route.hop_segments.is_empty()
                && route.violations.is_empty(),
            "directions require a legal street route without network hops"
        );
        ensure!(
            route.segments.as_ref().map(Vec::len) == Some(route.edge_path.len()),
            "directions require route segments"
        );
        let (phrasebook, warning) = Phrasebook::for_language(options.language.as_deref());
        let locale = phrasebook.locale();
        let warnings = warning.into_iter().collect();
        let (driving_side, driving_side_origin) = match options.driving_side {
            Some(side) => (side, DrivingSideOrigin::Request),
            // An explicit source wins; otherwise the dataset's admin areas
            // give the traffic side of the country the route starts in.
            None => match context
                .driving_side_source
                .and_then(|source| {
                    source.driving_side_at(route.origin.snapped_lon, route.origin.snapped_lat)
                })
                .or_else(|| {
                    route
                        .edge_path
                        .first()
                        .and_then(|&edge| self.topology().edge_driving_side(edge as usize))
                }) {
                Some(side) => (side, DrivingSideOrigin::Dataset),
                None => (DrivingSide::Right, DrivingSideOrigin::Default),
            },
        };
        let (graph, _) = self.routing_graph_for_mode(mode);
        let builder = maneuvers::ManeuverBuilder {
            topology: self.topology(),
            graph,
            names: context.names,
            route: &route,
            requested_destination: [request.destination.lon, request.destination.lat],
            driving_side,
            facility_attribute: options
                .facility_attribute
                .as_deref()
                .unwrap_or(DEFAULT_FACILITY_ATTRIBUTE),
            motor_vehicle: matches!(
                self.metrics().mode,
                TravelMode::Car
                    | TravelMode::Hgv
                    | TravelMode::Bus
                    | TravelMode::Taxi
                    | TravelMode::Motorcycle
                    | TravelMode::Moped
            ),
        };
        let drafts = builder.build();
        let maneuvers =
            narrative::render(&drafts, &route, locale, options.units, self.metrics().mode);
        Ok(DirectionsResult {
            language: locale.locale.clone(),
            units: options.units,
            driving_side,
            driving_side_origin,
            warnings,
            route,
            maneuvers,
        })
    }
}

#[cfg(test)]
mod tests;
