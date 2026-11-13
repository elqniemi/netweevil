//! Drawn walk geometry: width-adaptive clearance, corner shapes, door leads, validity.

use netweevil_indoor::geom::{P3, Rng};
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

fn at(x: f64, y: f64) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some("L0".into()),
        ..Default::default()
    }
}

fn space(s: &str) -> Loc {
    Loc {
        space: Some(s.into()),
        ..Default::default()
    }
}

fn go(r: &Router, p: &Profile, a: &Loc, b: &Loc) -> Route {
    let cp = r.profile(p);
    let mut ctx = r.ctx();
    r.route(&mut ctx, &cp, a, b).unwrap()
}

fn walk() -> Profile {
    Profile::builtin("walk").unwrap()
}

fn with_corner(c: Corner) -> Profile {
    Profile {
        corner: c,
        ..walk()
    }
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
    json!([[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]])
}

fn walk_pts(route: &Route) -> Vec<P3> {
    let mut v = vec![];
    for s in route.segments.iter().filter(|s| s.typ == "walk") {
        v.extend_from_slice(&route.coords[s.from_idx..=s.to_idx]);
    }
    v
}

/// Walk polylines sampled every 0.2 m.
fn walk_samples(route: &Route) -> Vec<P3> {
    let mut v = vec![];
    for s in route.segments.iter().filter(|s| s.typ == "walk") {
        for w in route.coords[s.from_idx..=s.to_idx].windows(2) {
            let l = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
            let n = (l / 0.2).ceil().max(1.0) as usize;
            for k in 0..=n {
                let t = k as f64 / n as f64;
                v.push([0, 1, 2].map(|i| w[0][i] + (w[1][i] - w[0][i]) * t));
            }
        }
    }
    v
}

/// Every walk vertex and sampled segment point lies on a walkable triangle.
fn assert_inside(r: &Router, route: &Route) {
    let g = &r.graph;
    let l = g.level_idx("L0").unwrap();
    for s in route.segments.iter().filter(|s| s.typ == "walk") {
        for w in route.coords[s.from_idx..=s.to_idx].windows(2) {
            for k in 0..=8 {
                let t = k as f64 / 8.0;
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

fn assert_durations(route: &Route) {
    let total: f64 = route.segments.iter().map(|s| s.duration).sum();
    assert!(
        (total - route.duration).abs() < 1e-6,
        "{total} {}",
        route.duration
    );
    let len: f64 = route.segments.iter().map(|s| s.length).sum();
    assert!((len - route.length).abs() < 1e-6);
}

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

/// A 30 x 2.4 m corridor with two rooms on its north side, doors at x = 5 and x = 23.
fn corridor_rooms() -> serde_json::Value {
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "c", "space": "c", "level": "L0", "class": "corridor", "polygon": [rect(0.0, 0.0, 30.0, 2.4)]},
            {"id": "A", "space": "A", "level": "L0", "class": "room", "polygon": [rect(2.0, 2.4, 8.0, 7.0)]},
            {"id": "B", "space": "B", "level": "L0", "class": "room", "polygon": [rect(20.0, 2.4, 26.0, 7.0)]}
        ],
        "portals": [
            {"id": "dA", "kind": "door", "segment": [[4.5, 2.4], [5.5, 2.4]], "level": "L0"},
            {"id": "dB", "kind": "door", "segment": [[22.5, 2.4], [23.5, 2.4]], "level": "L0"}
        ]
    })
}

#[test]
fn corridor_is_walked_in_the_middle() {
    let r = router(corridor_rooms());
    let route = go(&r, &walk(), &space("A"), &space("B"));
    assert_inside(&r, &route);
    assert_durations(&route);
    // Between the doors the walk keeps the corridor's middle band: 1.0 m (clearance_max)
    // from the wall it left, never closer than that to the far wall.
    let mid: Vec<P3> = walk_samples(&route)
        .into_iter()
        .filter(|p| p[1] < 2.4 && p[0] > 7.5 && p[0] < 20.5)
        .collect();
    assert!(!mid.is_empty());
    for p in &mid {
        assert!(
            (0.95..=1.45).contains(&p[1]),
            "{p:?} not in the middle band"
        );
    }
    // Doors are left and entered straight: the first and last 0.3 m beyond each door run
    // along its normal (vertical).
    for x in [5.0, 23.0] {
        let near: Vec<&P3> = route
            .coords
            .iter()
            .filter(|p| (p[1] - 2.4).abs() < 0.3)
            .filter(|p| (p[0] - x).abs() < 1.0)
            .collect();
        assert!(!near.is_empty());
        for p in near {
            assert!(
                (p[0] - x).abs() < 0.05,
                "{p:?}: not straight through door at x={x}"
            );
        }
    }
    // No zig-zags: every vertex turns less than 30 degrees (doors are entered with a fillet).
    let pts = walk_pts(&route);
    for w in pts.windows(3) {
        assert!(deflection(w[0], w[1], w[2]) < 0.53, "{w:?}");
    }
}

fn l_corridor() -> serde_json::Value {
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [{"id": "c", "space": "c", "level": "L0", "class": "corridor",
            "polygon": [[[0, 0], [20, 0], [20, 2], [2, 2], [2, 20], [0, 20], [0, 0]]]}]
    })
}

#[test]
fn l_turn_is_round_or_square_around_the_inner_corner() {
    let r = router(l_corridor());
    let (a, b) = (at(1.0, 18.0), at(18.0, 1.0));
    let corner = [2.0, 2.0];
    let d = |p: &P3| (p[0] - corner[0]).hypot(p[1] - corner[1]);

    let round = go(&r, &walk(), &a, &b);
    assert_inside(&r, &round);
    assert_durations(&round);
    let closest = round.coords.iter().map(d).fold(f64::MAX, f64::min);
    // 45% of the 2 m free width: 0.9 m from the corner.
    assert!((0.85..=1.2).contains(&closest), "{closest}");
    for w in round.coords.windows(3) {
        assert!(deflection(w[0], w[1], w[2]) < 0.3, "sharp turn {w:?}");
    }
    for p in &round.coords {
        if p[1] > 4.0 {
            assert!((0.85..=1.15).contains(&p[0]), "{p:?} off the middle");
        }
        if p[0] > 4.0 {
            assert!((0.85..=1.15).contains(&p[1]), "{p:?} off the middle");
        }
    }

    let square = go(&r, &with_corner(Corner::Square), &a, &b);
    assert_inside(&r, &square);
    let turns: Vec<(f64, P3)> = square
        .coords
        .windows(3)
        .map(|w| (deflection(w[0], w[1], w[2]), w[1]))
        .filter(|t| t.0 > 0.05)
        .collect();
    // One clean right angle where the two middle lines cross.
    assert_eq!(turns.len(), 1, "{:?}", square.coords);
    let (th, p) = turns[0];
    assert!((th - std::f64::consts::FRAC_PI_2).abs() < 0.1, "{th}");
    assert!((p[0] - 1.1).hypot(p[1] - 1.1) < 0.15, "{p:?}");

    let sharp = go(&r, &with_corner(Corner::Sharp), &a, &b);
    assert_inside(&r, &sharp);
    assert!(sharp.length <= square.length + 1e-6 && round.length <= square.length + 1e-6);
}

/// A 40 x 24 m hall with square pillars, a thin partition and a room behind a door.
fn hall() -> serde_json::Value {
    let mut holes = vec![];
    for i in 0..5 {
        for j in 0..2 {
            let (x, y) = (6.0 + 7.0 * i as f64, 7.0 + 9.0 * j as f64);
            holes.push(json!([
                [x, y],
                [x, y + 0.6],
                [x + 0.6, y + 0.6],
                [x + 0.6, y],
                [x, y]
            ]));
        }
    }
    // A partition wall (a thin hole) with a 1.2 m gap near its end.
    holes.push(json!([
        [10, 11.8],
        [10, 12.2],
        [30, 12.2],
        [30, 11.8],
        [10, 11.8]
    ]));
    let mut poly = vec![rect(0.0, 0.0, 40.0, 24.0)];
    poly.extend(holes);
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "hall", "space": "hall", "level": "L0", "class": "corridor", "polygon": poly},
            {"id": "room", "space": "room", "level": "L0", "class": "room", "polygon": [rect(40.0, 8.0, 46.0, 14.0)]}
        ],
        "portals": [{"id": "d", "kind": "door", "segment": [[40, 10.5], [40, 11.5]], "level": "L0"}]
    })
}

#[test]
fn random_walks_stay_inside_and_keep_clearance() {
    let v = hall();
    let r = router(v.clone());
    // Wall segments of the hall (outer ring and holes).
    let mut walls: Vec<([f64; 2], [f64; 2])> = vec![];
    for ring in v["surfaces"][0]["polygon"].as_array().unwrap() {
        let pts: Vec<[f64; 2]> = ring
            .as_array()
            .unwrap()
            .iter()
            .map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()])
            .collect();
        for w in pts.windows(2) {
            walls.push((w[0], w[1]));
        }
    }
    let wall_dist = |p: [f64; 2]| -> f64 {
        walls
            .iter()
            .map(|&(a, b)| {
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy))
                    .clamp(0.0, 1.0);
                (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
            })
            .fold(f64::MAX, f64::min)
    };
    let mut rng = Rng(0x5eed);
    let profiles = [
        walk(),
        Profile::builtin("wheelchair").unwrap(),
        Profile::builtin("bike").unwrap(),
        Profile {
            class_factor: Default::default(),
            tag_factor: Default::default(),
            ..Profile::builtin("bike").unwrap()
        },
        with_corner(Corner::Square),
        with_corner(Corner::Sharp),
    ];
    let (mut n, mut found) = (0, 0);
    let mut closest = f64::MAX;
    while n < 900 {
        let p = [rng.unit() * 40.0, rng.unit() * 24.0];
        let q = [rng.unit() * 40.0, rng.unit() * 24.0];
        if wall_dist(p) < 1.0 || wall_dist(q) < 1.0 {
            continue;
        }
        let prof = &profiles[n % profiles.len()];
        let cp = r.profile(prof);
        let mut ctx = r.ctx();
        let to = if n % 7 == 0 {
            space("room")
        } else {
            at(q[0], q[1])
        };
        n += 1;
        let Ok(route) = r.route(&mut ctx, &cp, &at(p[0], p[1]), &to) else {
            continue;
        };
        found += 1;
        assert_inside(&r, &route);
        assert_durations(&route);
        assert!(route.coords.iter().all(|c| c.iter().all(|x| x.is_finite())));
        // Away from the door, the walk keeps (nearly) the profile clearance from every wall.
        for c in walk_samples(&route) {
            if c[0] > 38.0 && (c[1] - 11.0).abs() < 2.0 || c[0] > 40.0 {
                continue;
            }
            let d = wall_dist([c[0], c[1]]);
            closest = closest.min(d / prof.clearance);
            assert!(
                d > 0.85 * prof.clearance,
                "{c:?}: {d} from a wall ({}, {p:?} -> {:?})",
                prof.name,
                to.at
            );
        }
    }
    eprintln!("{found} routes, closest approach {closest:.2} x clearance");
    assert!(found > 600, "{found}");
}

/// A bare 12 x 2 m corridor (two triangles) with rooms behind 0.2 m thick walls: both doors
/// open into the same long triangle against the wall, the corridor's middle into the other.
fn same_wall_doors() -> serde_json::Value {
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "c", "space": "c", "level": "L0", "class": "corridor", "polygon": [rect(0.0, 0.0, 12.0, 2.0)]},
            {"id": "A", "space": "A", "level": "L0", "class": "room", "polygon": [rect(1.0, 2.2, 5.0, 6.0)]},
            {"id": "B", "space": "B", "level": "L0", "class": "room", "polygon": [rect(7.0, 2.2, 11.0, 6.0)]}
        ],
        "portals": [
            {"id": "dA", "kind": "door", "segment": [[2.5, 2.1], [3.5, 2.1]], "level": "L0"},
            {"id": "dB", "kind": "door", "segment": [[8.5, 2.1], [9.5, 2.1]], "level": "L0"}
        ]
    })
}

#[test]
fn doors_on_one_wall_are_left_and_entered_straight() {
    let r = router(same_wall_doors());
    for (a, b) in [("A", "B"), ("B", "A")] {
        let route = go(&r, &walk(), &space(a), &space(b));
        assert_inside(&r, &route);
        assert_durations(&route);
        // Both doors are left along their normal to the corridor's middle (1 m from both
        // walls) instead of the walk running along the wall from door to door.
        let mid: Vec<P3> = walk_samples(&route)
            .into_iter()
            .filter(|p| p[1] < 2.0 && p[0] > 4.0 && p[0] < 8.0)
            .collect();
        assert!(!mid.is_empty());
        for p in &mid {
            assert!((0.9..=1.2).contains(&p[1]), "{a} -> {b}: {p:?}");
        }
    }
}

#[test]
fn min_heap_pops_in_order() {
    let mut h = netweevil_indoor::geom::MinHeap::new();
    let mut live: Vec<u64> = vec![];
    let mut rng = Rng(7);
    for i in 0..3000 {
        let k = rng.next_u64() >> 40; // small range: many equal keys
        h.push(k);
        live.push(k);
        if i % 3 == 0 {
            // Interleave pops with pushes, as a search does.
            let m = h.pop().unwrap();
            let j = (0..live.len()).min_by_key(|&j| live[j]).unwrap();
            assert_eq!(m, live.swap_remove(j));
        }
    }
    live.sort_unstable();
    let mut got = vec![];
    while let Some(k) = h.pop() {
        got.push(k);
    }
    assert_eq!(got, live);
    assert!(h.is_empty());
}
