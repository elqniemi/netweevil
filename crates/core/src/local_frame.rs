//! A local tangent-plane frame: site coordinates in metres around a WGS84
//! anchor.
//!
//! The forward map takes a point on the WGS84 ellipsoid to earth-centred
//! coordinates and then to east/north/up axes at the anchor; the site
//! coordinates are east and north, rotated by the frame's rotation. The
//! inverse finds the point on the ellipsoid whose east and north are the
//! given ones, so `to_lonlat(to_local(p)) == p` to well under a millimetre
//! over tens of kilometres.

use serde::{Deserialize, Serialize};

const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;
const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F);

/// The placement of a local metre frame on the globe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LocalFrame {
    /// Longitude of the frame origin, degrees.
    pub lon: f64,
    /// Latitude of the frame origin, degrees.
    pub lat: f64,
    /// Angle from east to the frame's x axis, counter-clockwise, degrees.
    #[serde(default)]
    pub rotation_deg: f64,
    /// Height of the frame's z = 0, added to local z on output, metres.
    #[serde(default)]
    pub elevation_m: f64,
}

impl LocalFrame {
    pub fn new(lon: f64, lat: f64) -> Self {
        Self {
            lon,
            lat,
            rotation_deg: 0.0,
            elevation_m: 0.0,
        }
    }

    fn origin_ecef(&self) -> [f64; 3] {
        geodetic_to_ecef(self.lon, self.lat, 0.0)
    }

    /// (east, north, up) unit vectors at the origin.
    fn axes(&self) -> [[f64; 3]; 3] {
        let (sl, cl) = self.lon.to_radians().sin_cos();
        let (sp, cp) = self.lat.to_radians().sin_cos();
        [
            [-sl, cl, 0.0],
            [-sp * cl, -sp * sl, cp],
            [cp * cl, cp * sl, sp],
        ]
    }

    /// Local `[x, y]` metres of a WGS84 point.
    pub fn to_local(&self, lon: f64, lat: f64) -> [f64; 2] {
        let p = geodetic_to_ecef(lon, lat, 0.0);
        let o = self.origin_ecef();
        let d = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
        let [e_axis, n_axis, _] = self.axes();
        let e = dot(d, e_axis);
        let n = dot(d, n_axis);
        let (s, c) = self.rotation_deg.to_radians().sin_cos();
        [e * c + n * s, -e * s + n * c]
    }

    /// WGS84 `[lon, lat]` of a local point.
    pub fn to_lonlat(&self, x: f64, y: f64) -> [f64; 2] {
        let (s, c) = self.rotation_deg.to_radians().sin_cos();
        let e = x * c - y * s;
        let n = x * s + y * c;
        let o = self.origin_ecef();
        let [e_axis, n_axis, u_axis] = self.axes();
        // The point on the ellipsoid lies below the tangent plane; move along
        // the up axis until its ellipsoidal height is zero.
        let mut u = -(e * e + n * n) / (2.0 * WGS84_A);
        let mut lonlat = [self.lon, self.lat];
        for _ in 0..6 {
            let p = [
                o[0] + e * e_axis[0] + n * n_axis[0] + u * u_axis[0],
                o[1] + e * e_axis[1] + n * n_axis[1] + u * u_axis[1],
                o[2] + e * e_axis[2] + n * n_axis[2] + u * u_axis[2],
            ];
            let (lon, lat, h) = ecef_to_geodetic(p);
            lonlat = [lon, lat];
            if h.abs() < 1e-7 {
                break;
            }
            // The local normal differs from the up axis by the angle between
            // them, so dividing by its cosine converges in a few steps.
            let normal = geodetic_normal(lon, lat);
            u -= h / dot(normal, u_axis).max(0.5);
        }
        lonlat
    }

    /// Local `[x, y]` of a direction given at a local point, mapped through a
    /// projection `f` by a small finite difference.
    pub fn transform_direction(
        f: impl Fn(f64, f64) -> [f64; 2],
        at: [f64; 2],
        dir: [f64; 2],
    ) -> [f64; 2] {
        let step = 0.5;
        let a = f(at[0], at[1]);
        let b = f(at[0] + dir[0] * step, at[1] + dir[1] * step);
        [(b[0] - a[0]) / step, (b[1] - a[1]) / step]
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn geodetic_normal(lon: f64, lat: f64) -> [f64; 3] {
    let (sl, cl) = lon.to_radians().sin_cos();
    let (sp, cp) = lat.to_radians().sin_cos();
    [cp * cl, cp * sl, sp]
}

fn geodetic_to_ecef(lon: f64, lat: f64, h: f64) -> [f64; 3] {
    let (sl, cl) = lon.to_radians().sin_cos();
    let (sp, cp) = lat.to_radians().sin_cos();
    let n = WGS84_A / (1.0 - WGS84_E2 * sp * sp).sqrt();
    [
        (n + h) * cp * cl,
        (n + h) * cp * sl,
        (n * (1.0 - WGS84_E2) + h) * sp,
    ]
}

/// (lon, lat, h) of an earth-centred point, by fixed-point iteration on the
/// latitude (converges to below 1e-12 rad in a few steps near the surface).
fn ecef_to_geodetic(p: [f64; 3]) -> (f64, f64, f64) {
    let lon = p[1].atan2(p[0]);
    let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
    let mut lat = p[2].atan2(r * (1.0 - WGS84_E2));
    let mut h = 0.0;
    for _ in 0..8 {
        let s = lat.sin();
        let n = WGS84_A / (1.0 - WGS84_E2 * s * s).sqrt();
        h = if lat.cos().abs() > 1e-9 {
            r / lat.cos() - n
        } else {
            p[2].abs() - n * (1.0 - WGS84_E2)
        };
        let next = p[2].atan2(r * (1.0 - WGS84_E2 * n / (n + h)));
        if (next - lat).abs() < 1e-14 {
            lat = next;
            break;
        }
        lat = next;
    }
    (lon.to_degrees(), lat.to_degrees(), h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::haversine_meters;

    #[test]
    fn round_trip_is_exact_over_kilometres() {
        for (lon, lat) in [(6.5665, 53.2194), (114.17, 22.30), (-70.6, -33.4)] {
            for rotation_deg in [0.0, 33.0, -120.0] {
                let frame = LocalFrame {
                    lon,
                    lat,
                    rotation_deg,
                    elevation_m: 0.0,
                };
                for (x, y) in [
                    (0.0, 0.0),
                    (12.5, -3.0),
                    (2500.0, 1800.0),
                    (-4000.0, 3000.0),
                ] {
                    let [plon, plat] = frame.to_lonlat(x, y);
                    let [bx, by] = frame.to_local(plon, plat);
                    assert!(
                        (bx - x).abs() < 1e-4 && (by - y).abs() < 1e-4,
                        "({x}, {y}) -> ({bx}, {by})"
                    );
                }
            }
        }
    }

    #[test]
    fn local_distances_match_the_ellipsoid() {
        let frame = LocalFrame::new(6.5665, 53.2194);
        let [lon, lat] = frame.to_lonlat(300.0, 400.0);
        let d = haversine_meters(frame.lon, frame.lat, lon, lat);
        // Haversine uses a sphere; agreement to 0.5 % confirms metres.
        assert!((d - 500.0).abs() < 2.5, "{d}");
        let rotated = LocalFrame {
            rotation_deg: 90.0,
            ..frame
        };
        // With the x axis pointing north, local +x is north.
        let [lon2, lat2] = rotated.to_lonlat(100.0, 0.0);
        assert!((lon2 - frame.lon).abs() < 1e-9 && lat2 > frame.lat);
    }
}
