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

/// Local equirectangular x-delta in metres at `reference_lat`.
pub fn projected_delta_x(from_lon: f64, reference_lat: f64, to_lon: f64) -> f64 {
    (to_lon - from_lon).to_radians() * reference_lat.to_radians().cos() * EARTH_RADIUS_M
}

/// Local equirectangular y-delta in metres.
pub fn projected_delta_y(from_lat: f64, to_lat: f64) -> f64 {
    (to_lat - from_lat).to_radians() * EARTH_RADIUS_M
}
