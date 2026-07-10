//! Shared geodesic helpers. Every crate uses the same spherical model with
//! this radius, so distances agree between ingest, routing, and simulation.

pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Great-circle distance in metres on the spherical earth model.
pub fn haversine_meters(from_lon: f64, from_lat: f64, to_lon: f64, to_lat: f64) -> f64 {
    let d_lat = (to_lat - from_lat).to_radians();
    let d_lon = (to_lon - from_lon).to_radians();
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let a =
        (d_lat / 2.0).sin().powi(2) + from_lat.cos() * to_lat.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

/// Three-dimensional distance between two longitude/latitude/elevation
/// coordinates. Elevation is interpreted as metres. If either elevation is
/// unknown (`NaN` or another non-finite value), this falls back to the
/// horizontal great-circle distance.
pub fn distance_3d_meters(
    from_lon: f64,
    from_lat: f64,
    from_z: f64,
    to_lon: f64,
    to_lat: f64,
    to_z: f64,
) -> f64 {
    let horizontal_m = haversine_meters(from_lon, from_lat, to_lon, to_lat);
    if from_z.is_finite() && to_z.is_finite() {
        horizontal_m.hypot(to_z - from_z)
    } else {
        horizontal_m
    }
}

/// Directional positive ascent and descent between two elevations. Unknown
/// elevations contribute neither ascent nor descent.
pub fn ascent_descent_meters(from_z: f64, to_z: f64) -> (f32, f32) {
    if !from_z.is_finite() || !to_z.is_finite() {
        return (0.0, 0.0);
    }
    let delta_z = (to_z - from_z) as f32;
    (delta_z.max(0.0), (-delta_z).max(0.0))
}

/// Local equirectangular x-delta in metres at `reference_lat`.
pub fn projected_delta_x(from_lon: f64, reference_lat: f64, to_lon: f64) -> f64 {
    (to_lon - from_lon).to_radians() * reference_lat.to_radians().cos() * EARTH_RADIUS_M
}

/// Local equirectangular y-delta in metres.
pub fn projected_delta_y(from_lat: f64, to_lat: f64) -> f64 {
    (to_lat - from_lat).to_radians() * EARTH_RADIUS_M
}

#[cfg(test)]
mod tests {
    use super::{ascent_descent_meters, distance_3d_meters};

    #[test]
    fn combines_horizontal_and_vertical_distance() {
        let distance = distance_3d_meters(0.0, 0.0, 10.0, 0.0, 0.0, 22.0);
        assert_eq!(distance, 12.0);
    }

    #[test]
    fn unknown_elevation_falls_back_to_horizontal_distance() {
        let horizontal = distance_3d_meters(6.5, 53.2, 0.0, 6.501, 53.201, 0.0);
        let unknown = distance_3d_meters(6.5, 53.2, f64::NAN, 6.501, 53.201, 100.0);
        assert_eq!(unknown, horizontal);
    }

    #[test]
    fn computes_directional_ascent_and_descent() {
        assert_eq!(ascent_descent_meters(10.0, 13.5), (3.5, 0.0));
        assert_eq!(ascent_descent_meters(13.5, 10.0), (0.0, 3.5));
        assert_eq!(ascent_descent_meters(f64::NAN, 10.0), (0.0, 0.0));
    }
}
