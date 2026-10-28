use super::*;

/// 100 m edge traversed in 20 s with 2.0 m/s² acceleration and 2.5 m/s² braking.
fn edge_distance(elapsed: f64) -> f64 {
    trapezoidal_distance(elapsed, 20.0, 100.0, 2.0, 2.5)
}

#[test]
fn trapezoid_covers_full_length() {
    let distance = edge_distance(20.0);
    assert!((distance - 100.0).abs() < 0.5, "got {distance}");
}

#[test]
fn trapezoid_monotonic() {
    let mut last = 0.0;
    for step in 0..=100 {
        let t = step as f64 * 0.2;
        let d = edge_distance(t);
        assert!(d + 1e-9 >= last, "not monotonic at t={t}");
        last = d;
    }
}

#[test]
fn trapezoid_slow_start_and_end() {
    // First second should cover less ground than a middle second.
    let first = edge_distance(1.0);
    let middle = edge_distance(10.0) - edge_distance(9.0);
    assert!(first < middle, "first={first} middle={middle}");
}

#[test]
fn degenerate_trapezoid_falls_back_to_linear() {
    // Too little time to accelerate: linear interpolation.
    let d = trapezoidal_distance(0.5, 1.0, 500.0, 1.0, 1.0);
    assert!((d - 250.0).abs() < 1.0);
}

#[test]
fn iso_offset_rolls_over_minutes() {
    assert_eq!(
        offset_iso_datetime("2026-01-01T08:00:00", 75.0),
        "2026-01-01T08:01:15"
    );
}
