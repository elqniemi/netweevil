use super::*;

#[test]
fn cost_and_link_encodings_round_trip_bit_for_bit() {
    let costs = [
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -0.0,
        1.0 / 1024.0,
        -15.0,
        f64::MIN_POSITIVE / 2.0,
        f64::MAX,
        f64::NAN,
    ];
    for cost in costs {
        assert_eq!(decode_cost(encode_cost(cost)).to_bits(), cost.to_bits());
    }
    assert_eq!(encode_cost(f64::INFINITY), 0);
    for link in [u32::MAX, 0, 1, u32::MAX - 1] {
        assert_eq!(decode_link(encode_link(link)), link);
    }
    assert_eq!(encode_link(u32::MAX), 0);
}

#[test]
fn small_search_dirties_only_touched_states_and_reset_clears_them() {
    let states = 1 << 22;
    let mut labels = SearchLabels::default();
    labels.prepare(states);
    assert_eq!(labels.cost.len(), states);
    assert!(labels.cost.get(states - 1).is_infinite());
    assert_eq!(labels.link.get(states - 1), u32::MAX);

    for (index, cost) in [(7, 3.0), (states - 1, 0.0), (1 << 20, -2.5), (7, 1.0)] {
        assert!(labels.update(index, cost, index as u32 + 1));
    }
    assert!(
        !labels.update(7, 1.0, 0),
        "an equal cost is not an improvement"
    );
    assert!(!labels.update(8, f64::INFINITY, 0));
    assert_eq!(labels.touched.len(), 3);
    assert_eq!(labels.cost.count_reached(), 3);
    assert_eq!(labels.link.count_linked(), 3);
    assert_eq!(labels.cost.get(7), 1.0);
    assert_eq!(labels.cost.get(states - 1), 0.0);
    assert_eq!(labels.link.get(1 << 20), (1 << 20) + 1);

    labels.prepare(states);
    assert!(labels.touched.is_empty());
    assert_eq!(labels.cost.len(), states);
    assert_eq!(labels.cost.count_reached(), 0);
    assert_eq!(labels.link.count_linked(), 0);
}

#[test]
fn large_search_releases_the_buffers() {
    let states = 64;
    let mut labels = SearchLabels::default();
    labels.prepare(states);
    for index in 0..=states / RELEASE_DIVISOR {
        labels.update(index, 1.0, 0);
    }
    assert!(release_after(states, labels.touched.len()));
    labels.prepare(states);
    assert_eq!(labels.touched.capacity(), 0);
    assert_eq!(labels.cost.len(), states);
    assert_eq!(labels.cost.count_reached(), 0);
    assert_eq!(labels.link.count_linked(), 0);
}

#[test]
fn snapshots_hold_the_touched_entries() {
    let states = 1024;
    let mut labels = SearchLabels::default();
    labels.prepare(states);
    labels.update(3, 2.0, 1);
    let (cost, link) = labels.take_snapshot();
    assert_eq!((cost.get(3), link.get(3)), (2.0, 1));
    assert_eq!(cost.count_reached(), 1);
    assert_eq!(labels.cost.get(3), 2.0, "a sparse snapshot copies");

    for index in 0..states {
        labels.update(index, 1.0, 0);
    }
    let (cost, link) = labels.take_snapshot();
    assert_eq!(cost.count_reached(), states);
    assert_eq!(link.get(5), 0);
    assert_eq!(labels.cost.len(), 0, "a dense snapshot moves the buffer");
    labels.prepare(states);
    assert_eq!(labels.cost.len(), states);
    assert_eq!(labels.cost.count_reached(), 0);
}
