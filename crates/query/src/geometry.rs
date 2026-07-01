pub(crate) fn interpolate_edge_point(
    from_lon: f64,
    from_lat: f64,
    to_lon: f64,
    to_lat: f64,
    fraction: f64,
) -> [f64; 2] {
    [
        from_lon + (to_lon - from_lon) * fraction,
        from_lat + (to_lat - from_lat) * fraction,
    ]
}

pub(crate) fn buffered_segment_corners(
    start: [f64; 2],
    end: [f64; 2],
    buffer_width_m: f64,
    at_lat: f64,
) -> Vec<[f64; 2]> {
    let dx_m = longitude_delta_to_meters(end[0] - start[0], at_lat);
    let dy_m = latitude_delta_to_meters(end[1] - start[1]);
    let length_m = (dx_m * dx_m + dy_m * dy_m).sqrt();
    let (unit_px, unit_py) = if length_m <= 1e-6 {
        (0.0, 1.0)
    } else {
        (-dy_m / length_m, dx_m / length_m)
    };
    let offset_lon = meters_to_longitude_delta(unit_px * buffer_width_m, at_lat);
    let offset_lat = meters_to_latitude_delta(unit_py * buffer_width_m);
    vec![
        [start[0] + offset_lon, start[1] + offset_lat],
        [start[0] - offset_lon, start[1] - offset_lat],
        [end[0] - offset_lon, end[1] - offset_lat],
        [end[0] + offset_lon, end[1] + offset_lat],
    ]
}

pub(crate) fn convex_hull(mut points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    points.sort_by(|left, right| {
        left[0]
            .total_cmp(&right[0])
            .then_with(|| left[1].total_cmp(&right[1]))
    });
    points.dedup_by(|left, right| {
        (left[0] - right[0]).abs() <= 1e-12 && (left[1] - right[1]).abs() <= 1e-12
    });

    if points.len() <= 1 {
        return points;
    }

    let mut lower = Vec::<[f64; 2]>::new();
    for point in &points {
        while lower.len() >= 2
            && cross(lower[lower.len() - 2], lower[lower.len() - 1], *point) <= 0.0
        {
            lower.pop();
        }
        lower.push(*point);
    }

    let mut upper = Vec::<[f64; 2]>::new();
    for point in points.iter().rev() {
        while upper.len() >= 2
            && cross(upper[upper.len() - 2], upper[upper.len() - 1], *point) <= 0.0
        {
            upper.pop();
        }
        upper.push(*point);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

pub(crate) fn simplify_polygon_ring(
    mut ring: Vec<[f64; 2]>,
    tolerance_m: f64,
) -> Option<Vec<[f64; 2]>> {
    if ring.is_empty() {
        return None;
    }
    if tolerance_m > 0.0 {
        let mut simplified = Vec::new();
        for point in ring {
            if simplified.last().is_none_or(|previous: &[f64; 2]| {
                haversine_meters(previous[0], previous[1], point[0], point[1]) >= tolerance_m
            }) {
                simplified.push(point);
            }
        }
        ring = simplified;
    }
    if ring.len() == 1 {
        let point = ring[0];
        let offset_lon = meters_to_longitude_delta(5.0, point[1]);
        let offset_lat = meters_to_latitude_delta(5.0);
        ring = vec![
            [point[0] - offset_lon, point[1] - offset_lat],
            [point[0] + offset_lon, point[1] - offset_lat],
            [point[0] + offset_lon, point[1] + offset_lat],
            [point[0] - offset_lon, point[1] + offset_lat],
        ];
    } else if ring.len() == 2 {
        let corners =
            buffered_segment_corners(ring[0], ring[1], 5.0, (ring[0][1] + ring[1][1]) / 2.0);
        ring = convex_hull(corners);
    }
    if ring.len() < 3 {
        return None;
    }
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    Some(ring)
}

fn longitude_delta_to_meters(delta_lon: f64, at_lat: f64) -> f64 {
    delta_lon * 111_320.0 * at_lat.to_radians().cos().abs().max(0.01)
}

fn latitude_delta_to_meters(delta_lat: f64) -> f64 {
    delta_lat * 110_540.0
}

fn meters_to_longitude_delta(meters: f64, at_lat: f64) -> f64 {
    meters / (111_320.0 * at_lat.to_radians().cos().abs().max(0.01))
}

fn meters_to_latitude_delta(meters: f64) -> f64 {
    meters / 110_540.0
}

pub(crate) use netweevil_core::geo::projected_delta_x;

pub(crate) use netweevil_core::geo::{haversine_meters, projected_delta_y};
