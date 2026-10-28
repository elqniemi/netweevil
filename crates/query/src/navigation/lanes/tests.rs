use super::*;
use LaneIndication as I;

fn flags(lanes: &[Lane]) -> (String, String) {
    let render = |pick: fn(&Lane) -> bool| {
        lanes
            .iter()
            .map(|lane| if pick(lane) { 'x' } else { '.' })
            .collect::<String>()
    };
    (render(|lane| lane.valid), render(|lane| lane.active))
}

#[test]
fn parses_lanes_left_to_right_with_unmarked_lanes() {
    assert_eq!(
        parse_turn_lanes("left|through;right||none"),
        Some(vec![
            vec![I::Left],
            vec![I::Through, I::Right],
            vec![I::None],
            vec![I::None],
        ])
    );
    assert_eq!(
        parse_turn_lanes("none;left|through"),
        Some(vec![vec![I::Left], vec![I::Through]])
    );
    // Unknown values and a single unmarked lane carry no guidance.
    assert_eq!(parse_turn_lanes("left|bus"), None);
    assert_eq!(parse_turn_lanes("none"), None);
}

#[test]
fn valid_and_active_lanes_follow_the_movement_and_the_next_maneuver() {
    use LaneMovement::*;
    for (value, movement, next, valid, active) in [
        ("left|through|through;right", Left, None, "x..", "x.."),
        ("left|through|through;right", Through, None, ".xx", ".xx"),
        ("left|through|through;right", Right, None, "..x", "..x"),
        // A right turn soon after going straight: stay in the right lane.
        (
            "left|through|through;right",
            Through,
            Some(Right),
            ".xx",
            "..x",
        ),
        (
            "left|through|through;right",
            Through,
            Some(Left),
            ".xx",
            ".x.",
        ),
        // Unmarked lanes go straight.
        ("left||", Through, None, ".xx", ".xx"),
        // No slight_right arrow: fall back to right.
        ("through|right", SlightRight, None, ".x", ".x"),
        // Motorway exit: the main road keeps the through lanes.
        (
            "through|through|through;slight_right|slight_right",
            KeepLeft,
            None,
            "xxx.",
            "xxx.",
        ),
        (
            "through|through|through;slight_right|slight_right",
            KeepRight,
            None,
            "..xx",
            "..xx",
        ),
        // U-turn in right-hand traffic uses the leftmost turn lanes.
        (
            "left|left;through|right",
            Reverse { left: true },
            None,
            "xx.",
            "xx.",
        ),
        (
            "reverse;left|left|through",
            Reverse { left: true },
            None,
            "x..",
            "x..",
        ),
        // No lane leads there.
        ("left|through", Right, None, "..", ".."),
    ] {
        let lanes = parse_turn_lanes(value).unwrap();
        let guidance = lane_guidance(&lanes, movement, next);
        assert_eq!(
            flags(&guidance),
            (valid.to_string(), active.to_string()),
            "{value} {movement:?} then {next:?}"
        );
    }
}

#[test]
fn valid_lanes_report_the_indication_they_use() {
    let lanes = parse_turn_lanes("left||through").unwrap();
    let guidance = lane_guidance(&lanes, LaneMovement::Through, None);
    assert_eq!(
        guidance
            .iter()
            .map(|lane| lane.valid_indication)
            .collect::<Vec<_>>(),
        [None, Some(I::None), Some(I::Through)]
    );
}
