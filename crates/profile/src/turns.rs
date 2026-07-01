use netweevil_core::{EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, TopologyBundle};

pub(crate) fn transition_turn_penalty_cost(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
    left_penalty_s: f64,
    right_penalty_s: f64,
    uturn_penalty_s: f64,
    traffic_signal_penalty_s: f64,
    roundabout_entry_penalty_s: f64,
    cost_time_weight: f64,
) -> f64 {
    transition_turn_penalty_seconds(
        topology,
        previous_edge_index,
        next_edge_index,
        left_penalty_s,
        right_penalty_s,
        uturn_penalty_s,
        traffic_signal_penalty_s,
        roundabout_entry_penalty_s,
    ) * cost_time_weight
}

fn transition_turn_penalty_seconds(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
    left_penalty_s: f64,
    right_penalty_s: f64,
    uturn_penalty_s: f64,
    traffic_signal_penalty_s: f64,
    roundabout_entry_penalty_s: f64,
) -> f64 {
    let previous = topology.routing_edge(previous_edge_index);
    let next = topology.routing_edge(next_edge_index);

    if previous.to != next.from {
        return 0.0;
    }

    let mut penalty_s = 0.0;
    if previous.flags & EDGE_FLAG_TARGET_TRAFFIC_SIGNAL != 0 {
        penalty_s += traffic_signal_penalty_s;
    }
    if previous.flags & EDGE_FLAG_ROUNDABOUT == 0 && next.flags & EDGE_FLAG_ROUNDABOUT != 0 {
        penalty_s += roundabout_entry_penalty_s;
    }

    if previous.from == next.to {
        return penalty_s + uturn_penalty_s;
    }

    if previous.source_way_id == next.source_way_id {
        return penalty_s;
    }

    penalty_s
        + match classify_turn(topology, previous_edge_index, next_edge_index) {
            TurnDirection::Straight => 0.0,
            TurnDirection::Left => left_penalty_s,
            TurnDirection::Right => right_penalty_s,
            TurnDirection::Uturn => uturn_penalty_s,
        }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnDirection {
    Straight,
    Left,
    Right,
    Uturn,
}

fn classify_turn(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> TurnDirection {
    const STRAIGHT_THRESHOLD_RAD: f64 = 30.0_f64.to_radians();
    const UTURN_THRESHOLD_RAD: f64 = 150.0_f64.to_radians();

    let previous = topology.routing_edge(previous_edge_index);
    let next = topology.routing_edge(next_edge_index);
    let from = &topology.nodes[previous.from.0 as usize];
    let via = &topology.nodes[previous.to.0 as usize];
    let to = &topology.nodes[next.to.0 as usize];

    let in_x = projected_delta_x(from.lon, via.lat, via.lon);
    let in_y = projected_delta_y(from.lat, via.lat);
    let out_x = projected_delta_x(via.lon, via.lat, to.lon);
    let out_y = projected_delta_y(via.lat, to.lat);

    let in_norm = (in_x * in_x + in_y * in_y).sqrt();
    let out_norm = (out_x * out_x + out_y * out_y).sqrt();
    if in_norm <= f64::EPSILON || out_norm <= f64::EPSILON {
        return TurnDirection::Straight;
    }

    let dot = ((in_x * out_x + in_y * out_y) / (in_norm * out_norm)).clamp(-1.0, 1.0);
    let cross = in_x * out_y - in_y * out_x;
    let angle = cross.atan2(dot);
    let abs_angle = angle.abs();

    if abs_angle <= STRAIGHT_THRESHOLD_RAD {
        TurnDirection::Straight
    } else if abs_angle >= UTURN_THRESHOLD_RAD {
        TurnDirection::Uturn
    } else if angle > 0.0 {
        TurnDirection::Left
    } else {
        TurnDirection::Right
    }
}

use netweevil_core::geo::{projected_delta_x, projected_delta_y};
