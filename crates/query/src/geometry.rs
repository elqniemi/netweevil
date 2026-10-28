pub(crate) fn interpolate_edge_point_3d(from: [f64; 3], to: [f64; 3], fraction: f64) -> [f64; 3] {
    [
        from[0] + (to[0] - from[0]) * fraction,
        from[1] + (to[1] - from[1]) * fraction,
        from[2] + (to[2] - from[2]) * fraction,
    ]
}

pub(crate) use netweevil_core::geo::projected_delta_x;

pub(crate) use netweevil_core::geo::{haversine_meters, projected_delta_y};
