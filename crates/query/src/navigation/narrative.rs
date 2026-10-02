//! Written and spoken instruction text for detected maneuvers.

use netweevil_core::{HighwayClass, TravelMode};

use super::maneuvers::Draft;
use super::phrases::{CARDINAL_KEYS, Locale};
use super::{DistanceUnits, Maneuver, ManeuverType, Side, VerbalInstructions};
use crate::RouteResult;

/// Stairs, escalators and lifts that climb or drop less than this are
/// described without a direction.
const LEVEL_CHANGE_MIN_M: f64 = 0.5;

/// Spoken timing per travel mode.
struct VerbalTiming {
    /// A maneuver this close after the previous one is announced with it.
    then_within_m: u64,
    /// Shortest previous step that still gets a separate pre-alert.
    min_pre_alert_m: u64,
}

fn timing(mode: TravelMode) -> VerbalTiming {
    match mode {
        TravelMode::Foot => VerbalTiming {
            then_within_m: 25,
            min_pre_alert_m: 40,
        },
        TravelMode::Bicycle => VerbalTiming {
            then_within_m: 40,
            min_pre_alert_m: 80,
        },
        TravelMode::Car | TravelMode::Hgv | TravelMode::Transit => VerbalTiming {
            then_within_m: 60,
            min_pre_alert_m: 150,
        },
    }
}

/// How far ahead a maneuver is announced on the approach road.
fn pre_alert_lead_m(mode: TravelMode, approach: Option<HighwayClass>) -> u64 {
    match mode {
        TravelMode::Foot => 50,
        TravelMode::Bicycle => 150,
        TravelMode::Car | TravelMode::Hgv | TravelMode::Transit => match approach {
            Some(
                HighwayClass::Motorway
                | HighwayClass::MotorwayLink
                | HighwayClass::Trunk
                | HighwayClass::TrunkLink,
            ) => 1_500,
            Some(
                HighwayClass::Primary
                | HighwayClass::PrimaryLink
                | HighwayClass::Secondary
                | HighwayClass::SecondaryLink,
            ) => 750,
            _ => 400,
        },
    }
}

fn cardinal_key(bearing: f64) -> &'static str {
    CARDINAL_KEYS[((bearing.rem_euclid(360.0) + 22.5) / 45.0) as usize % CARDINAL_KEYS.len()]
}

const fn side_key(left: bool) -> &'static str {
    if left { "left" } else { "right" }
}

const fn direction_key(kind: ManeuverType) -> &'static str {
    match kind {
        ManeuverType::SlightLeft => "slight_left",
        ManeuverType::Left => "left",
        ManeuverType::SharpLeft => "sharp_left",
        ManeuverType::SlightRight => "slight_right",
        ManeuverType::Right => "right",
        _ => "sharp_right",
    }
}

/// The instruction as a sentence fragment.
pub(super) fn fragment(draft: &Draft, locale: &Locale) -> String {
    use ManeuverType as K;
    let street = draft.street.label();
    let street = street.as_deref();
    let signs = draft.signs.as_ref();
    let toward = signs
        .map(|signs| {
            let mut items = signs.branch.clone();
            items.extend(signs.toward.iter().cloned());
            locale.list(&items)
        })
        .filter(|text| !text.is_empty());
    let toward = toward.as_deref();
    let exit_number = signs
        .map(|signs| signs.exit_number.join("/"))
        .filter(|text| !text.is_empty());
    let exit_number = exit_number.as_deref();
    let with_street = |plain: &str, named: &str| match street {
        Some(street) => locale.fragment(named, &[("street", street)]),
        None => locale.fragment(plain, &[]),
    };
    match draft.kind {
        K::Depart => {
            let cardinal = locale.cardinal(cardinal_key(draft.bearing_after.unwrap_or(0.0)));
            match street {
                Some(street) => locale.fragment(
                    "depart_street",
                    &[("cardinal", cardinal), ("street", street)],
                ),
                None => locale.fragment("depart", &[("cardinal", cardinal)]),
            }
        }
        K::Continue => with_street("continue", "continue_street"),
        K::SlightLeft | K::Left | K::SharpLeft | K::SlightRight | K::Right | K::SharpRight => {
            let direction = locale.direction(direction_key(draft.kind));
            match (street, toward) {
                (Some(street), _) => locale.fragment(
                    "turn_street",
                    &[("direction", direction), ("street", street)],
                ),
                (None, Some(toward)) => locale.fragment(
                    "turn_toward",
                    &[("direction", direction), ("toward", toward)],
                ),
                (None, None) => locale.fragment("turn", &[("direction", direction)]),
            }
        }
        K::UturnLeft | K::UturnRight => with_street("uturn", "uturn_street"),
        K::RampLeft | K::RampRight => {
            let side = locale.side(side_key(draft.kind == K::RampLeft));
            match toward {
                Some(toward) => {
                    locale.fragment("ramp_toward", &[("side", side), ("toward", toward)])
                }
                None => locale.fragment("ramp", &[("side", side)]),
            }
        }
        K::RampStraight => match toward {
            Some(toward) => locale.fragment("ramp_straight_toward", &[("toward", toward)]),
            None => locale.fragment("ramp_straight", &[]),
        },
        K::ExitLeft | K::ExitRight => {
            let side = locale.side(side_key(draft.kind == K::ExitLeft));
            match (exit_number, toward) {
                (Some(exit), Some(toward)) => locale.fragment(
                    "exit_number_toward",
                    &[("exit", exit), ("side", side), ("toward", toward)],
                ),
                (Some(exit), None) => {
                    locale.fragment("exit_number", &[("exit", exit), ("side", side)])
                }
                (None, Some(toward)) => {
                    locale.fragment("exit_toward", &[("side", side), ("toward", toward)])
                }
                (None, None) => locale.fragment("exit", &[("side", side)]),
            }
        }
        K::KeepLeft | K::KeepRight => {
            let side = locale.side(side_key(draft.kind == K::KeepLeft));
            match (toward, street) {
                (Some(toward), _) => {
                    locale.fragment("keep_toward", &[("side", side), ("toward", toward)])
                }
                (None, Some(street)) => {
                    locale.fragment("keep_street", &[("side", side), ("street", street)])
                }
                (None, None) => locale.fragment("keep", &[("side", side)]),
            }
        }
        K::KeepStraight => match (toward, street) {
            (Some(toward), _) => locale.fragment("keep_straight_toward", &[("toward", toward)]),
            (None, Some(street)) => locale.fragment("keep_straight_street", &[("street", street)]),
            (None, None) => locale.fragment("keep_straight", &[]),
        },
        K::Merge | K::MergeLeft | K::MergeRight => with_street("merge", "merge_street"),
        K::EnterRoundabout => match draft.roundabout_exit_count {
            Some(count) => {
                let ordinal = locale.ordinal(count);
                match draft.exit_street.label() {
                    Some(street) => locale.fragment(
                        "roundabout_enter_exit_street",
                        &[("ordinal", &ordinal), ("street", &street)],
                    ),
                    None => locale.fragment("roundabout_enter_exit", &[("ordinal", &ordinal)]),
                }
            }
            None => locale.fragment("roundabout_enter", &[]),
        },
        K::ExitRoundabout => with_street("roundabout_exit", "roundabout_exit_street"),
        K::FerryEnter => with_street("ferry_enter", "ferry_enter_street"),
        K::FerryExit => locale.fragment("ferry_exit", &[]),
        K::Stairs | K::Escalator | K::Elevator => {
            let base = match draft.kind {
                K::Stairs => "stairs",
                K::Escalator => "escalator",
                _ => "elevator",
            };
            let key = match draft.level_change_m {
                Some(change) if change >= LEVEL_CHANGE_MIN_M => format!("{base}_up"),
                Some(change) if change <= -LEVEL_CHANGE_MIN_M => format!("{base}_down"),
                _ => base.to_string(),
            };
            locale.fragment(&key, &[])
        }
        K::Travelator => locale.fragment("travelator", &[]),
        K::Arrive => match draft.arrival_side {
            Some(side) => locale.fragment(
                "arrive_side",
                &[("side", locale.side(side_key(side == Side::Left)))],
            ),
            None => locale.fragment("arrive", &[]),
        },
    }
}

pub(super) fn render(
    drafts: &[Draft],
    route: &RouteResult,
    locale: &Locale,
    units: DistanceUnits,
    mode: TravelMode,
) -> Vec<Maneuver> {
    let segments = route.segments.as_deref().unwrap_or_default();
    let timing = timing(mode);
    let fragments = drafts
        .iter()
        .map(|draft| fragment(draft, locale))
        .collect::<Vec<_>>();
    let spans = drafts
        .iter()
        .map(|draft| {
            segments[draft.begin..draft.end]
                .iter()
                .map(|segment| u64::from(segment.length_m))
                .sum::<u64>()
        })
        .collect::<Vec<_>>();
    drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            let span = &segments[draft.begin..draft.end];
            let next = drafts.get(index + 1);
            let announce_next = next.is_some_and(|next| next.kind != ManeuverType::Arrive)
                && spans[index] <= timing.then_within_m;
            let alert = if announce_next {
                locale.sentence(&locale.fragment(
                    "verbal_then",
                    &[
                        ("instruction", &fragments[index]),
                        ("next", &fragments[index + 1]),
                    ],
                ))
            } else {
                locale.sentence(&fragments[index])
            };
            let pre_alert_distance_m = index
                .checked_sub(1)
                .map(|previous| spans[previous])
                .filter(|&previous| previous >= timing.min_pre_alert_m)
                .map(|previous| previous.min(pre_alert_lead_m(mode, draft.approach_highway)));
            let pre_alert = pre_alert_distance_m.map(|distance| {
                let distance = locale.distance(distance as f64, units, true);
                locale.sentence(&if draft.kind == ManeuverType::Arrive {
                    locale.fragment("arrive_pre_alert", &[("distance", &distance)])
                } else {
                    locale.fragment(
                        "verbal_pre_alert",
                        &[("distance", &distance), ("instruction", &fragments[index])],
                    )
                })
            });
            let post_transition = (draft.kind != ManeuverType::Arrive
                && spans[index] > timing.then_within_m)
                .then(|| {
                    let distance = locale.distance(spans[index] as f64, units, false);
                    locale.sentence(
                        &locale.fragment("verbal_post_transition", &[("distance", &distance)]),
                    )
                });
            Maneuver {
                kind: draft.kind,
                instruction: locale.sentence(&fragments[index]),
                verbal: VerbalInstructions {
                    pre_alert,
                    pre_alert_distance_m: pre_alert_distance_m
                        .map(|distance| u32::try_from(distance).unwrap_or(u32::MAX)),
                    alert,
                    post_transition,
                },
                street_name: draft.street.name.clone(),
                street_ref: draft.street.reference.clone(),
                location: draft.location,
                begin_edge_index: draft.begin,
                end_edge_index: draft.end,
                bearing_before: draft.bearing_before,
                bearing_after: draft.bearing_after,
                distance_m: spans[index],
                edge_time_s: span.iter().map(|segment| segment.travel_time_s).sum(),
                waiting_time_s: span.iter().map(|segment| segment.waiting_time_s).sum(),
                // Arrival has no edges; it begins when the last one ends.
                begin_time: match span.first() {
                    Some(segment) => segment.entry_time.clone(),
                    None => draft
                        .begin
                        .checked_sub(1)
                        .and_then(|last| segments.get(last))
                        .and_then(|segment| segment.exit_time.clone()),
                },
                roundabout_exit_count: draft.roundabout_exit_count,
                arrival_side: draft.arrival_side,
                level_change_m: draft.level_change_m,
                signs: draft.signs.clone(),
                lanes: draft.lanes.clone(),
            }
        })
        .collect()
}
