//! Behavioural tests on small inline fixtures.

use netweevil_indoor::geom::P3;
use netweevil_indoor::profile::Corner;
use netweevil_indoor::{Input, Loc, Profile, Route, Router, compile};
use serde_json::json;

fn router(v: serde_json::Value) -> Router {
    let input: Input = serde_json::from_value(v).unwrap();
    let (g, rep) = compile(&input);
    for w in &rep.warnings {
        eprintln!("warning: {w}");
    }
    Router::new(g)
}

fn at(x: f64, y: f64, level: &str) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(level.into()),
        ..Default::default()
    }
}

fn go(r: &Router, p: &Profile, a: &Loc, b: &Loc) -> Result<Route, String> {
    let cp = r.profile(p);
    let mut ctx = r.ctx();
    r.route(&mut ctx, &cp, a, b)
}

fn walk() -> Profile {
    Profile::builtin("walk").unwrap()
}
fn wheelchair() -> Profile {
    Profile::builtin("wheelchair").unwrap()
}
fn ids(r: &Route) -> Vec<&str> {
    r.segments.iter().map(|s| s.id.as_str()).collect()
}
fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]])
}

/// Every coordinate and every segment midpoint of walk segments lies on a walkable triangle.
fn assert_inside(r: &Router, route: &Route, level: &str) {
    let g = &r.graph;
    let l = g.level_idx(level).unwrap();
    for s in route.segments.iter().filter(|s| s.typ == "walk") {
        let pts = &route.coords[s.from_idx..=s.to_idx];
        for w in pts.windows(2) {
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let p = [
                    w[0][0] + (w[1][0] - w[0][0]) * t,
                    w[0][1] + (w[1][1] - w[0][1]) * t,
                ];
                let ok = g.tri_at(l, p).is_some() || g.nearest_tri(l, p, 1e-6, |_| true).is_some();
                assert!(ok, "point {p:?} of {} outside walkable space", s.id);
            }
        }
    }
}

/// Heading change (radians) at b on the path a -> b -> c.
fn deflection(a: P3, b: P3, c: P3) -> f64 {
    let (d1, d2) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
    let l = d1[0].hypot(d1[1]) * d2[0].hypot(d2[1]);
    if l < 1e-12 {
        return 0.0;
    }
    ((d1[0] * d2[0] + d1[1] * d2[1]) / l)
        .clamp(-1.0, 1.0)
        .acos()
}

fn two_rooms() -> serde_json::Value {
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "A", "space": "A", "level": "L0", "class": "room", "polygon": rect(0.0, 0.0, 5.0, 4.0)},
            {"id": "B", "space": "B", "level": "L0", "class": "room", "polygon": rect(5.0, 0.0, 10.0, 4.0)}
        ],
        "portals": [{"id": "d1", "kind": "door", "segment": [[5, 1], [5, 2]], "level": "L0", "width": 0.9}]
    })
}

#[test]
fn two_rooms_route_goes_through_door_not_wall() {
    let r = router(two_rooms());
    let route = go(&r, &walk(), &at(1.0, 3.5, "L0"), &at(9.0, 3.5, "L0")).unwrap();
    assert!(ids(&route).contains(&"d1"));
    assert_eq!(route.spaces, vec!["A", "B"]);
    // The path crosses the wall x = 5 only within the door.
    for w in route.coords.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a[0] - 5.0) * (b[0] - 5.0) < 0.0 {
            let y = a[1] + (b[1] - a[1]) * (5.0 - a[0]) / (b[0] - a[0]);
            assert!((1.0..=2.0).contains(&y), "crosses wall at y={y}");
        }
    }
    // Lead-in: the door is approached along its normal (horizontal segment through it).
    let k = route
        .coords
        .iter()
        .position(|p| p[0] > 4.4 && p[0] < 5.0)
        .expect("lead point before door");
    assert!((route.coords[k][1] - 1.5).abs() < 1e-6);
    assert_inside(&r, &route, "L0");
    // Doorless: no route.
    let mut v = two_rooms();
    v["portals"] = json!([]);
    assert!(
        go(
            &router(v),
            &walk(),
            &at(1.0, 3.5, "L0"),
            &at(9.0, 3.5, "L0")
        )
        .is_err()
    );
}

fn u_corridor() -> serde_json::Value {
    // U-shaped corridor, 2 m wide, around a 6 x 8 core.
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [{"id": "c", "space": "c", "level": "L0", "class": "corridor",
            "polygon": [[[0, 0], [10, 0], [10, 12], [8, 12], [8, 2], [2, 2], [2, 12], [0, 12], [0, 0]]]}]
    })
}

#[test]
fn corridor_corners_rounded_and_inside() {
    let r = router(u_corridor());
    let (a, b) = (at(1.0, 11.0, "L0"), at(9.0, 11.0, "L0"));
    let round = go(&r, &walk(), &a, &b).unwrap();
    let mut sharp_p = walk();
    sharp_p.corner = Corner::Sharp;
    let sharp = go(&r, &sharp_p, &a, &b).unwrap();
    assert_inside(&r, &round, "L0");
    assert_inside(&r, &sharp, "L0");
    // Sharp: two real turns, at the inner corners (2,2), (8,2); elsewhere the path bends by a
    // few degrees at most to stay in the corridor's middle band.
    let turns: Vec<f64> = sharp
        .coords
        .windows(3)
        .map(|w| deflection(w[0], w[1], w[2]))
        .collect();
    assert_eq!(
        turns.iter().filter(|&&t| t > 0.2).count(),
        2,
        "{:?}",
        sharp.coords
    );
    assert!(
        turns.iter().all(|&t| !(0.2..=0.6).contains(&t)),
        "{turns:?}"
    );
    // Round: fillet arcs add points; every turn is gentle.
    assert!(round.coords.len() > 8, "{:?}", round.coords);
    for w in round.coords.windows(3) {
        let (d1, d2) = (
            [w[1][0] - w[0][0], w[1][1] - w[0][1]],
            [w[2][0] - w[1][0], w[2][1] - w[1][1]],
        );
        let c = (d1[0] * d2[0] + d1[1] * d2[1]) / (d1[0].hypot(d1[1]) * d2[0].hypot(d2[1]));
        assert!(c.acos() < 0.6, "sharp turn in rounded path");
    }
    // Keeps clearance from the inner corners.
    for p in &sharp.coords {
        for c in [[2.0, 2.0], [8.0, 2.0]] {
            assert!((p[0] - c[0]).hypot(p[1] - c[1]) > 0.2);
        }
    }
    // Near the centreline (28 m with square corners), arcs cut the corners a little.
    assert!(
        round.length > 25.0 && round.length < 28.0,
        "{}",
        round.length
    );
    // In the corridor legs the walk keeps to the middle band, 0.9 m (45% of 2 m) from walls.
    for p in &round.coords {
        if p[1] > 3.0 && p[1] < 10.0 {
            let x = if p[0] < 5.0 { p[0] } else { 10.0 - p[0] };
            assert!((0.85..=1.15).contains(&x), "{p:?} off the middle");
        }
    }
}

fn two_floors() -> serde_json::Value {
    let mut surfaces = vec![];
    let mut portals = vec![];
    for (l, z) in [("L0", 0.0), ("L1", 4.0)] {
        surfaces.push(json!({"id": format!("{l}_c"), "space": format!("{l}_c"), "level": l, "class": "corridor", "polygon": rect(0.0, 0.0, 30.0, 3.0)}));
        surfaces.push(json!({"id": format!("{l}_r"), "space": format!("{l}_r"), "level": l, "class": "room", "polygon": rect(10.0, 3.0, 16.0, 8.0)}));
        portals.push(
            json!({"id": format!("{l}_d"), "segment": [[12.5, 3], [13.5, 3]], "level": l, "z": z}),
        );
    }
    json!({
        "levels": [{"id": "L0", "z": 0.0}, {"id": "L1", "z": 4.0}],
        "surfaces": surfaces, "portals": portals,
        "verticals": [
            {"id": "stairs", "kind": "stairs", "stops": [{"level": "L0", "at": [2, 1.5]}, {"level": "L1", "at": [2, 1.5]}],
             "line": [[2, 1.5, 0], [8, 1.5, 4]]},
            {"id": "lift", "kind": "lift", "wait": 20, "stops": [{"level": "L0", "at": [28, 1.5]}, {"level": "L1", "at": [28, 1.5]}]}
        ]
    })
}

#[test]
fn stairs_for_walk_lift_for_wheelchair() {
    let r = router(two_floors());
    let (a, b) = (
        at(5.0, 1.5, "L0"),
        Loc {
            space: Some("L1_r".into()),
            ..Default::default()
        },
    );
    let w = go(&r, &walk(), &a, &b).unwrap();
    assert!(
        ids(&w).contains(&"stairs") && !ids(&w).contains(&"lift"),
        "{:?}",
        ids(&w)
    );
    assert!((w.climb - 4.0).abs() < 1e-6);
    let c = go(&r, &wheelchair(), &a, &b).unwrap();
    assert!(
        ids(&c).contains(&"lift") && !ids(&c).contains(&"stairs"),
        "{:?}",
        ids(&c)
    );
    assert!(c.duration > 20.0);
    assert_eq!(c.spaces.last().unwrap(), "L1_r");
    // 3D point (no level) lands on the upper floor.
    let up = Loc {
        at: Some(vec![20.0, 1.5, 4.0]),
        ..Default::default()
    };
    let u = go(&r, &walk(), &a, &up).unwrap();
    assert!((u.coords.last().unwrap()[2] - 4.0).abs() < 1e-6);
}

fn plaza_with_barrier(pen: f64) -> serde_json::Value {
    json!({
        "levels": [{"id": "S", "z": 0.0}],
        "surfaces": [{"id": "plaza", "space": "plaza", "level": "S", "class": "plaza", "walls": false, "polygon": rect(0.0, 0.0, 40.0, 20.0)}],
        "barriers": [{"id": "fence", "line": [[0, 10], [40, 10]], "level": "S"}],
        "portals": [{"id": "x", "kind": "crossing", "segment": [[30, 10], [33, 10]], "level": "S", "penalty": pen}]
    })
}

#[test]
fn barrier_crossed_only_at_crossing_with_penalty() {
    let (a, b) = (at(5.0, 5.0, "S"), at(5.0, 15.0, "S"));
    let r0 = go(&router(plaza_with_barrier(0.0)), &walk(), &a, &b).unwrap();
    let r1 = go(&router(plaza_with_barrier(10.0)), &walk(), &a, &b).unwrap();
    assert!(ids(&r0).contains(&"x"));
    for w in r0.coords.windows(2) {
        if (w[0][1] - 10.0) * (w[1][1] - 10.0) < 0.0 {
            let x = w[0][0] + (w[1][0] - w[0][0]) * (10.0 - w[0][1]) / (w[1][1] - w[0][1]);
            assert!((30.0..=33.0).contains(&x), "crossed fence at x={x}");
        }
    }
    assert!(r0.length > 50.0, "{}", r0.length);
    assert!(
        ((r1.duration - r0.duration) - 10.0).abs() < 1e-3,
        "{} vs {}",
        r1.duration,
        r0.duration
    );
    assert_inside(&router(plaza_with_barrier(0.0)), &r0, "S");
}

fn net() -> serde_json::Value {
    json!({
        "levels": [{"id": "G", "z": 0.0}],
        "surfaces": [{"id": "hall", "space": "hall", "level": "G", "class": "corridor", "polygon": rect(0.0, 0.0, 10.0, 10.0)}],
        "edges": [
            {"id": "in", "line": [[5, 5], [20, 5]], "level": "G", "class": "footway"},
            {"id": "road", "line": [[20, 5], [100, 5]], "level": "G", "class": "footway"},
            {"id": "short_oneway", "line": [[100, 5], [100, 30]], "level": "G", "oneway": true, "class": "footway"},
            {"id": "long_back", "line": [[100, 5], [130, 5], [130, 30], [100, 30]], "level": "G", "class": "footway"},
            {"id": "steep", "line": [[20, 5, 0], [20, 15, 1.2]], "level": "G", "class": "footway"},
            {"id": "gentle", "line": [[20, 5, 0], [40, 5, 0.5], [40, 15, 1.2], [20, 15, 1.2]], "level": "G", "class": "footway"},
            {"id": "service_cut", "line": [[20, 15, 1.2], [20, 25, 1.2]], "level": "G", "class": "service"},
            {"id": "detour", "line": [[20, 15, 1.2], [5, 20, 1.2], [20, 25, 1.2]], "level": "G", "class": "footway"}
        ]
    })
}

#[test]
fn edges_attach_to_surface() {
    let r = router(net());
    let route = go(&r, &walk(), &at(2.0, 2.0, "G"), &at(60.0, 5.0, "G")).unwrap();
    let i = ids(&route);
    assert!(
        i.contains(&"hall") && i.contains(&"in") && i.contains(&"road"),
        "{i:?}"
    );
    assert_eq!(route.spaces, vec!["hall"]);
    assert!((route.coords.last().unwrap()[0] - 60.0).abs() < 1e-9);
    // From a node id.
    assert!(
        go(
            &r,
            &walk(),
            &at(2.0, 2.0, "G"),
            &Loc {
                at: Some(vec![100.0, 30.0]),
                ..Default::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn oneway_edge_respected() {
    let r = router(net());
    let (a, b) = (at(100.0, 6.0, "G"), at(100.0, 29.0, "G"));
    let fwd = go(&r, &walk(), &a, &b).unwrap();
    assert!(ids(&fwd).contains(&"short_oneway") && fwd.length < 25.0);
    // Backwards it may only ride the oneway forwards: up to (100,30), round long_back, then
    // forwards again from (100,5) to the target.
    let back = go(&r, &walk(), &b, &a).unwrap();
    assert_eq!(
        ids(&back),
        vec!["short_oneway", "long_back", "short_oneway"]
    );
    assert!(back.length > 85.0, "{}", back.length);
    for w in back.coords.windows(2) {
        if w[0][0] == 100.0 && w[1][0] == 100.0 {
            assert!(w[1][1] >= w[0][1], "against the oneway: {w:?}");
        }
    }
}

#[test]
fn incline_max_forbids_steep_ramp_for_wheelchair() {
    let r = router(net());
    let (a, b) = (at(20.0, 5.0, "G"), at(20.0, 15.0, "G"));
    let w = go(&r, &walk(), &a, &b).unwrap();
    assert_eq!(ids(&w), vec!["steep"]);
    let c = go(&r, &wheelchair(), &a, &b).unwrap();
    assert!(
        ids(&c).contains(&"gentle") && !ids(&c).contains(&"steep"),
        "{:?}",
        ids(&c)
    );
    // Downhill is too steep as well.
    let d = go(&r, &wheelchair(), &b, &a).unwrap();
    assert!(!ids(&d).contains(&"steep"));
}

#[test]
fn forbidden_class_is_avoided() {
    let r = router(net());
    let (a, b) = (at(20.0, 15.0, "G"), at(20.0, 25.0, "G"));
    assert_eq!(ids(&go(&r, &walk(), &a, &b).unwrap()), vec!["service_cut"]);
    let mut p = walk();
    p.class_factor.insert("service".into(), None);
    let route = go(&r, &p, &a, &b).unwrap();
    assert_eq!(ids(&route), vec!["detour"]);
    // Everything forbidden: no route.
    p.class_factor.insert("footway".into(), None);
    assert!(go(&r, &p, &a, &b).is_err());
}

#[test]
fn graph_roundtrip_and_space_locate() {
    let r = router(two_rooms());
    let path = std::env::temp_dir().join("netweevil_indoor_test.graph");
    netweevil_indoor::io::save(&r.graph, path.to_str().unwrap()).unwrap();
    let r2 = Router::new(netweevil_indoor::io::load(path.to_str().unwrap()).unwrap());
    let a = Loc {
        space: Some("A".into()),
        ..Default::default()
    };
    let b = Loc {
        portal: Some("d1".into()),
        ..Default::default()
    };
    let x = go(&r2, &walk(), &a, &b).unwrap();
    assert_eq!(x.spaces, vec!["A"]);
    let _: P3 = x.coords[0];
}

/// `at` with `space`: the spot, kept inside that space. B has a notch by the shared wall, so
/// a point in the notch is nearer room A's floor than B's; plain `at` snaps into A, `at` +
/// `space` stays in B. Out of reach it falls back to the space's usual point.
#[test]
fn point_in_space_snaps_inside_the_space() {
    let v = json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "A", "space": "A", "level": "L0", "class": "room", "polygon": rect(0.0, 0.0, 5.0, 4.0)},
            {"id": "B", "space": "B", "level": "L0", "class": "room",
             "polygon": [[[5.0, 0.0], [10.0, 0.0], [10.0, 4.0], [6.0, 4.0], [6.0, 3.0], [5.0, 3.0], [5.0, 0.0]]]}
        ],
        "portals": [{"id": "d1", "kind": "door", "segment": [[5, 1], [5, 2]], "level": "L0", "width": 0.9}]
    });
    let r = router(v);
    let start = at(1.0, 1.0, "L0");
    let in_b = |x: f64, y: f64| Loc {
        space: Some("B".into()),
        ..at(x, y, "L0")
    };
    let plain = go(&r, &walk(), &start, &at(5.2, 3.5, "L0")).unwrap();
    assert_eq!(plain.spaces, vec!["A"]);
    let kept = go(&r, &walk(), &start, &in_b(5.2, 3.5)).unwrap();
    assert_eq!(kept.spaces, vec!["A", "B"]);
    let end = *kept.coords.last().unwrap();
    assert!(
        end[0] >= 5.0 && end[0] < 6.2 && end[1] <= 3.0 + 1e-6,
        "end {end:?}"
    );
    // A spot on B's floor is the exact end.
    let exact = go(&r, &walk(), &start, &in_b(8.0, 3.5)).unwrap();
    let end = *exact.coords.last().unwrap();
    assert!(
        (end[0] - 8.0).abs() < 1e-6 && (end[1] - 3.5).abs() < 1e-6,
        "end {end:?}"
    );
    // Out of reach: the space's own point, still in B.
    let far = go(&r, &walk(), &start, &in_b(60.0, 60.0)).unwrap();
    assert_eq!(far.spaces, vec!["A", "B"]);
}

/// Bike: an indoor room reached from an outdoor plaza. The ride outside runs at bike speed,
/// the walk inside at the `tag_speed` cap, and the route exists at all (indoor is allowed).
#[test]
fn bike_rides_outside_and_walks_inside() {
    let v = json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "plaza", "level": "L0", "class": "plaza", "walls": false, "tags": ["outdoor"],
             "polygon": rect(0.0, 0.0, 40.0, 10.0)},
            {"id": "shop", "space": "shop", "level": "L0", "class": "room", "tags": ["indoor"],
             "polygon": rect(40.0, 0.0, 60.0, 10.0)}
        ],
        "portals": [{"id": "door", "kind": "door", "segment": [[40, 4], [40, 6]], "level": "L0", "width": 1.2}]
    });
    let r = router(v);
    let bike = Profile::builtin("bike").unwrap();
    let route = go(&r, &bike, &at(2.0, 5.0, "L0"), &at(58.0, 5.0, "L0")).unwrap();
    assert!(ids(&route).contains(&"door"));
    let time = |id: &str| -> (f64, f64) {
        route
            .segments
            .iter()
            .filter(|s| s.typ == "walk" && s.space.as_deref().unwrap_or(s.id.as_str()) == id)
            .fold((0.0, 0.0), |(l, t), s| (l + s.length, t + s.duration))
    };
    let (out_len, out_t) = time("plaza");
    let (in_len, in_t) = time("shop");
    assert!(
        out_len > 30.0 && in_len > 10.0,
        "outside {out_len} m, inside {in_len} m"
    );
    assert!(
        (out_len / out_t - 4.5).abs() < 0.3,
        "riding speed {}",
        out_len / out_t
    );
    assert!(
        (in_len / in_t - 1.3).abs() < 0.15,
        "walking speed {}",
        in_len / in_t
    );
    // Without `on_foot` the bike may not go inside at all.
    let mut alone = bike.clone();
    alone.on_foot = None;
    assert!(go(&r, &alone, &at(2.0, 5.0, "L0"), &at(58.0, 5.0, "L0")).is_err());
    // A `tag_speed` cap instead (ridden slowly inside) still works.
    let mut capped = alone.clone();
    capped.tag_factor.remove("indoor");
    capped.tag_speed.insert("indoor".into(), 1.3);
    let slow = go(&r, &capped, &at(2.0, 5.0, "L0"), &at(58.0, 5.0, "L0")).unwrap();
    assert!((slow.duration - route.duration).abs() < 0.25 * route.duration);
}
