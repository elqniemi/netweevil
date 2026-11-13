//! Edge cases of compilation, costs and graph files (small inline fixtures).

use netweevil_indoor::geom::P3;
use netweevil_indoor::graph::{FactKind, K_ZONE};
use netweevil_indoor::{
    CompileReport, CompiledProfile, Input, Loc, Profile, Route, Router, compile, io,
};
use serde_json::{Value, json};

fn build(v: Value) -> (Router, CompileReport) {
    let input: Input = serde_json::from_value(v).unwrap();
    let (g, rep) = compile(&input);
    for w in &rep.warnings {
        eprintln!("warning: {w}");
    }
    (Router::new(g), rep)
}

fn router(v: Value) -> Router {
    build(v).0
}

fn at(x: f64, y: f64, level: &str) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(level.into()),
        ..Default::default()
    }
}

fn node(id: &str) -> Loc {
    Loc {
        node: Some(id.into()),
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

fn prof(v: Value) -> Profile {
    Profile::resolve(&v.to_string()).unwrap()
}

fn ids(r: &Route) -> Vec<&str> {
    r.segments.iter().map(|s| s.id.as_str()).collect()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]])
}

fn walk_facts(r: &Router) -> usize {
    r.graph
        .facts
        .iter()
        .filter(|f| f.kind == FactKind::Walk)
        .count()
}

/// Max heading change (radians) at interior route vertices, ignoring vertical steps.
fn max_turn(c: &[P3]) -> f64 {
    let pts: Vec<P3> = c.iter().copied().fold(vec![], |mut v: Vec<P3>, p| {
        if v.last()
            .is_none_or(|q| (q[0] - p[0]).hypot(q[1] - p[1]) > 1e-6)
        {
            v.push(p);
        }
        v
    });
    let mut m: f64 = 0.0;
    for w in pts.windows(3) {
        let (a, b) = (
            [w[1][0] - w[0][0], w[1][1] - w[0][1]],
            [w[2][0] - w[1][0], w[2][1] - w[1][1]],
        );
        let c = (a[0] * b[0] + a[1] * b[1]) / (a[0].hypot(a[1]) * b[0].hypot(b[1]));
        m = m.max(c.clamp(-1.0, 1.0).acos());
    }
    m
}

// Big zones (> K + 1 nodes) keep at most K walks per node and stay connected.

/// 60 nodes at each end of a long plaza: every node's K nearest are on its own side.
fn knn(holes: bool) -> Value {
    let mut nodes = vec![];
    for k in 0..60 {
        let (dx, dy) = ((k % 10) as f64 * 0.5, (k / 10) as f64 * 0.5);
        nodes.push(json!({"id": format!("L{k}"), "at": [1.0 + dx, 1.0 + dy], "level": "L0"}));
        nodes.push(json!({"id": format!("R{k}"), "at": [194.0 + dx, 1.0 + dy], "level": "L0"}));
    }
    let mut poly = vec![json!([[0, 0], [200, 0], [200, 20], [0, 20]])];
    if holes {
        // Small holes make many triangles, so the triangle Dijkstra settles nodes one by one.
        for i in 0..20 {
            for j in 0..4 {
                let (x, y) = (20.0 + i as f64 * 8.0, 2.0 + j as f64 * 5.0);
                poly.push(json!([
                    [x, y],
                    [x + 0.1, y],
                    [x + 0.1, y + 0.1],
                    [x, y + 0.1]
                ]));
            }
        }
    }
    json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [{"id": "plaza", "level": "L0", "walls": false, "polygon": poly}],
        "nodes": nodes
    })
}

#[test]
fn big_zone_knn_is_connected() {
    for holes in [false, true] {
        let r = router(knn(holes));
        let route = go(&r, &walk(), &node("L0"), &node("R0")).unwrap();
        assert!(
            route.length > 190.0 && route.length < 200.0,
            "{}",
            route.length
        );
        assert!(walk_facts(&r) <= 120 * K_ZONE, "{} walks", walk_facts(&r));
    }
}

#[test]
fn big_zone_compiles_fast() {
    // 40 x 40 street grid on one open plaza: 1600 nodes in a zone of a few triangles.
    let mut edges = vec![];
    for i in 0..40 {
        let c = 5.0 + i as f64 * 10.0;
        edges.push(json!({"line": (0..40).map(|k| [5.0 + k as f64 * 10.0, c]).collect::<Vec<_>>(), "level": "L0"}));
        edges.push(json!({"line": (0..40).map(|k| [c, 5.0 + k as f64 * 10.0]).collect::<Vec<_>>(), "level": "L0"}));
    }
    let t = std::time::Instant::now();
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [{"id": "plaza", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 400.0, 400.0)}],
        "edges": edges
    }));
    assert!(t.elapsed().as_secs_f64() < 10.0);
    assert!(walk_facts(&r) <= 1600 * K_ZONE);
    let route = go(&r, &walk(), &at(1.0, 1.0, "L0"), &at(399.0, 399.0, "L0")).unwrap();
    assert!(route.length < 600.0);
}

// Edge nodes do not attach through walls; exterior doors (dangling portal ends) first.

fn office(door: bool) -> Value {
    let mut v = json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [{"id": "office", "space": "office", "class": "room", "level": "L0", "walls": true,
                      "polygon": rect(0.0, 0.0, 10.0, 10.0)}],
        "edges": [
            {"id": "path", "line": [[30, 5], [20, 5], [10.6, 5]], "level": "L0", "class": "footway"},
            {"id": "path2", "line": [[20, 5], [20, 20]], "level": "L0", "class": "footway"}
        ]
    });
    if door {
        v["portals"] = json!([{"id": "entrance", "segment": [[10, 4.5], [10, 5.5]], "level": "L0",
                               "oneway": "backward", "width": 0.7, "penalty": 5}]);
    }
    v
}

#[test]
fn edge_attaches_through_exterior_door_only() {
    let r = router(office(true));
    let route = go(&r, &walk(), &at(20.0, 20.0, "L0"), &at(5.0, 5.0, "L0")).unwrap();
    assert!(ids(&route).contains(&"entrance"), "{:?}", ids(&route));
    // The door is one-way (inwards): no way out through the wall either.
    assert!(go(&r, &walk(), &at(5.0, 5.0, "L0"), &at(20.0, 20.0, "L0")).is_err());
}

#[test]
fn edge_near_walled_room_without_door_is_not_attached() {
    let (r, rep) = build(office(false));
    assert!(go(&r, &walk(), &at(20.0, 20.0, "L0"), &at(5.0, 5.0, "L0")).is_err());
    assert!(rep.warnings.iter().any(|w| w.msg.contains("not attached")));
}

#[test]
fn edge_ending_inside_room_still_attaches() {
    // Entrance edge ending 0.35 m inside the room, and a zero-length room link.
    let mut v = office(false);
    v["edges"][0]["line"] = json!([[30, 5], [20, 5], [9.65, 5]]);
    v["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "link", "line": [[5, 9.5], [5, 9.5]], "level": "L0"}));
    let r = router(v);
    let route = go(&r, &walk(), &at(20.0, 20.0, "L0"), &at(5.0, 5.0, "L0")).unwrap();
    assert!(ids(&route).contains(&"path"));
}

// Neighbouring surfaces at different z keep their own height (no sloped strips).

#[test]
fn surfaces_keep_their_own_z() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "A", "space": "A", "level": "L0", "z": 0.0, "polygon": rect(0.0, 0.0, 5.0, 4.0)},
            {"id": "B", "space": "B", "level": "L0", "z": 0.3, "polygon": rect(5.0, 0.0, 6.0, 4.0)},
            {"id": "C", "space": "C", "level": "L0", "z": 0.3, "polygon": rect(6.0, 0.0, 12.0, 4.0)}
        ],
        "portals": [
            {"id": "d1", "segment": [[5, 1.5], [5, 2.5]], "level": "L0"},
            {"id": "d2", "segment": [[6, 1.5], [6, 2.5]], "level": "L0"}
        ]
    }));
    let route = go(&r, &walk(), &at(1.0, 2.0, "L0"), &at(11.0, 2.0, "L0")).unwrap();
    for s in route.segments.iter().filter(|s| s.typ == "walk") {
        let want = if s.id == "A" { 0.0 } else { 0.3 };
        for p in &route.coords[s.from_idx..=s.to_idx] {
            assert!((p[2] - want).abs() < 1e-9, "{} at z {}", s.id, p[2]);
        }
    }
    assert!((route.climb - 0.3).abs() < 1e-6);
}

// Barrier vertices do not drag the surface to the level z.

#[test]
fn barrier_does_not_warp_surface() {
    for lvl in [Some("street"), None] {
        let mut bar = json!({"id": "fence", "line": [[10, 5], [10, 15]]});
        if let Some(l) = lvl {
            bar["level"] = json!(l);
        }
        let r = router(json!({
            "levels": [{"id": "street", "z": 0}],
            "surfaces": [{"id": "sq", "level": "street", "walls": false,
                          "polygon": [[[0, 0, 10], [20, 0, 10], [20, 20, 10], [0, 20, 10]]]}],
            "barriers": [bar]
        }));
        let route = go(
            &r,
            &walk(),
            &at(5.0, 10.0, "street"),
            &at(15.0, 10.0, "street"),
        )
        .unwrap();
        assert!(route.coords.iter().all(|p| (p[2] - 10.0).abs() < 1e-9));
        assert!(
            route.length > 14.0,
            "went through the fence: {}",
            route.length
        );
        assert_eq!(route.climb, 0.0);
    }
}

// Walk pruning respects profiles (a witness over grass does not replace a footway walk).

#[test]
fn pruning_keeps_walks_needed_by_profiles() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "fw", "class": "footway", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 22.0, 2.0)},
            {"id": "grass", "class": "grass", "level": "L0", "walls": false, "polygon": rect(0.0, 2.0, 22.0, 10.0)}
        ],
        "edges": [
            {"id": "ei", "line": [[1, 1], [1, -5]], "level": "L0"},
            {"id": "ej", "line": [[21, 1], [21, -5]], "level": "L0"},
            {"id": "ev", "line": [[11, 2.4], [11, 9]], "level": "L0"}
        ]
    }));
    let nograss = prof(json!({"name": "nograss", "speed": 1.3, "class_factor": {"grass": null}}));
    let route = go(&r, &nograss, &at(1.0, -4.0, "L0"), &at(21.0, -4.0, "L0")).unwrap();
    assert_eq!(route.spaces.len(), 0);
    assert!(route.segments.iter().all(|s| s.id != "grass"));
    assert!(route.length < 31.0);
}

// Portals drawn inside one room, and one-way semantics in fallback directions.

fn offset_door(spaces: Option<Value>) -> Value {
    let mut d = json!({"id": "d1", "segment": [[5.0, 1.5], [5.0, 2.5]], "level": "L0"});
    if let Some(s) = spaces {
        d["spaces"] = s;
    }
    json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "A", "space": "A", "level": "L0", "polygon": rect(0.0, 0.0, 5.1, 4.0)},
            {"id": "B", "space": "B", "level": "L0", "polygon": rect(5.2, 0.0, 10.0, 4.0)}
        ],
        "portals": [d]
    })
}

#[test]
fn portal_inside_one_room_reaches_the_other() {
    for spaces in [None, Some(json!(["A", "B"])), Some(json!(["B", "A"]))] {
        let r = router(offset_door(spaces));
        let route = go(&r, &walk(), &at(1.0, 2.0, "L0"), &at(9.0, 2.0, "L0")).unwrap();
        assert_eq!(route.spaces, vec!["A", "B"]);
        assert!(ids(&route).contains(&"d1"));
    }
}

#[test]
fn oneway_kept_in_fallback_directions() {
    // Door point inside A whose normal runs along the room: both sides along it are A, so the
    // 12 fallback directions decide; "forward" still means along the given normal (towards A).
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "A", "space": "A", "level": "L0", "polygon": rect(0.0, 0.0, 5.0, 4.0)},
            {"id": "B", "space": "B", "level": "L0", "polygon": rect(5.0, 0.0, 10.0, 4.0)}
        ],
        "portals": [{"id": "d1", "at": [4.8, 2.0], "width": 0.9, "normal": [-0.1, -1.0],
                     "level": "L0", "oneway": "forward"}]
    }));
    assert!(go(&r, &walk(), &at(9.0, 2.0, "L0"), &at(1.0, 2.0, "L0")).is_ok());
    assert!(go(&r, &walk(), &at(1.0, 2.0, "L0"), &at(9.0, 2.0, "L0")).is_err());
}

// 2D mode (levels without z): escalator direction by stop order, lift time per floor.

fn two_d(kind: &str, levels: usize) -> Value {
    let lv: Vec<Value> = (0..levels)
        .map(|i| json!({"id": format!("L{i}")}))
        .collect();
    let surfs: Vec<Value> = (0..levels)
        .map(|i| json!({"id": format!("f{i}"), "level": format!("L{i}"), "walls": false, "polygon": rect(0.0, 0.0, 10.0, 10.0)}))
        .collect();
    let stops: Vec<Value> = (0..levels)
        .map(|i| json!({"level": format!("L{i}"), "at": [5, 5]}))
        .collect();
    json!({"levels": lv, "surfaces": surfs,
           "verticals": [{"id": "v", "kind": kind, "direction": "up", "stops": stops}]})
}

#[test]
fn escalator_direction_without_z() {
    let r = router(two_d("escalator", 2));
    let up = go(&r, &walk(), &at(5.0, 4.0, "L0"), &at(5.0, 4.0, "L1")).unwrap();
    assert!(ids(&up).contains(&"v"));
    assert!(go(&r, &walk(), &at(5.0, 4.0, "L1"), &at(5.0, 4.0, "L0")).is_err());
}

#[test]
fn lift_time_scales_with_floors_without_z() {
    let r = router(two_d("lift", 3));
    let seg = |to: &str| -> f64 {
        let route = go(&r, &walk(), &at(5.0, 4.0, "L0"), &at(5.0, 4.0, to)).unwrap();
        route
            .segments
            .iter()
            .find(|s| s.id == "v")
            .unwrap()
            .duration
    };
    let (one, two) = (seg("L1"), seg("L2"));
    assert!((one - (30.0 + 3.5)).abs() < 1e-3, "{one}");
    assert!((two - (30.0 + 7.0)).abs() < 1e-3, "{two}");
}

// The heuristic stays exact with zero-time links and short fast arcs.

#[test]
fn astar_exact_with_teleport_edge() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "a", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 40.0, 40.0)},
            {"id": "b", "level": "L0", "walls": false, "polygon": rect(1000.0, 0.0, 1040.0, 40.0)}
        ],
        "edges": [
            {"id": "ferry", "line": [[20, 20], [1020, 20]], "level": "L0", "time": 0},
            {"id": "road", "line": [[39, 39], [1001, 39]], "level": "L0"},
            {"id": "hop", "line": [[5, 5], [5.2, 5]], "level": "L0", "time": 0}
        ]
    }));
    let cp = CompiledProfile::new(&r.graph, &walk());
    let mut cp0 = CompiledProfile::new(&r.graph, &walk());
    cp0.inv_vmax = 0.0;
    assert!(cp.inv_vmax > 0.0 && cp.reach >= 1000.0);
    let mut ctx = r.ctx();
    for (a, b) in [
        ((2.0, 2.0), (1038.0, 38.0)),
        ((30.0, 5.0), (1010.0, 30.0)),
        ((5.0, 30.0), (35.0, 10.0)),
    ] {
        let (x, y) = (at(a.0, a.1, "L0"), at(b.0, b.1, "L0"));
        let r1 = r.route(&mut ctx, &cp, &x, &y).unwrap();
        let r0 = r.route(&mut ctx, &cp0, &x, &y).unwrap();
        assert!(
            (r1.cost - r0.cost).abs() < 1e-3 * r0.cost.max(1.0),
            "{} vs {}",
            r1.cost,
            r0.cost
        );
    }
}

// Small z offsets across a door are not slopes.

#[test]
fn door_z_offset_is_not_a_slope() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "A", "space": "A", "level": "L0", "z": 0.0, "polygon": rect(0.0, 0.0, 4.9, 4.0)},
            {"id": "B", "space": "B", "level": "L0", "z": 0.05, "polygon": rect(5.1, 0.0, 10.0, 4.0)}
        ],
        "portals": [{"id": "d1", "segment": [[5, 1.5], [5, 2.5]], "level": "L0", "width": 1.0}]
    }));
    let wc = Profile::builtin("wheelchair").unwrap();
    assert!(go(&r, &wc, &at(1.0, 2.0, "L0"), &at(9.0, 2.0, "L0")).is_ok());
}

// Profiles are validated.

#[test]
fn invalid_profiles_are_rejected() {
    for bad in [
        json!({"speed": 0}),
        json!({"speed": -1}),
        json!({"class_factor": {"room": -1}}),
        json!({"kind_penalty": {"door": -2}}),
        json!({"incline": {"up": -1}}),
        json!({"turn_penalty": -0.5}),
    ] {
        let e = Profile::resolve(&bad.to_string());
        assert!(e.is_err(), "{bad} accepted");
    }
    assert!(
        Profile::resolve(&json!({"speed": 1.2, "class_factor": {"road": null}}).to_string())
            .is_ok()
    );
}

// Snapping prefers locations the profile can use; without level / z, the ground.

#[test]
fn snap_prefers_usable_surface_and_ground_level() {
    let r = router(json!({
        "levels": [{"id": "up", "z": 4}, {"id": "ground", "z": 0}],
        "surfaces": [
            {"id": "fw", "class": "footway", "level": "ground", "walls": false, "polygon": rect(0.0, 0.0, 20.0, 2.0)},
            {"id": "grass", "class": "grass", "level": "ground", "walls": false, "polygon": rect(0.0, 2.0, 20.0, 10.0)},
            {"id": "deck", "level": "up", "walls": false, "polygon": rect(0.0, 0.0, 20.0, 10.0)}
        ]
    }));
    let nograss = prof(json!({"name": "nograss", "class_factor": {"grass": null}}));
    let route = go(
        &r,
        &nograss,
        &at(5.0, 3.0, "ground"),
        &at(15.0, 1.0, "ground"),
    )
    .unwrap();
    assert_eq!(route.segments[0].id, "fw");
    let free = |x: f64, y: f64| Loc {
        at: Some(vec![x, y]),
        ..Default::default()
    };
    let route = go(&r, &walk(), &free(5.0, 5.0), &free(15.0, 5.0)).unwrap();
    assert_eq!(route.segments[0].level.as_deref(), Some("ground"));
}

#[test]
fn attach_widens_past_blocked_nodes() {
    // 30 path ends lie in a grass patch next to the query point (unreachable without grass);
    // the only usable node is far away along the footway.
    let mut edges: Vec<Value> = (0..30)
        .map(|i| json!({"line": [[1.0 + i as f64 * 0.1, 5.0], [1.0 + i as f64 * 0.1, 30.0]], "level": "L0"}))
        .collect();
    edges.push(json!({"id": "fw", "line": [[90, 1], [90, -20]], "level": "L0"}));
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "path", "class": "footway", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 100.0, 2.0)},
            {"id": "grass", "class": "grass", "level": "L0", "walls": false, "polygon": rect(0.0, 2.0, 100.0, 10.0)}
        ],
        "edges": edges
    }));
    let nograss = prof(json!({"name": "nograss", "class_factor": {"grass": null}}));
    let route = go(&r, &nograss, &at(1.0, 1.0, "L0"), &at(90.0, -15.0, "L0")).unwrap();
    assert!(ids(&route).contains(&"fw"));
}

// Walk durations are split by surface speed, not length.

#[test]
fn walk_duration_split_by_surface_speed() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "slow", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 10.0, 4.0)},
            {"id": "fast", "level": "L0", "walls": false, "speed": 2.6, "polygon": rect(10.0, 0.0, 20.0, 4.0)}
        ]
    }));
    let route = go(&r, &walk(), &at(1.0, 2.0, "L0"), &at(19.0, 2.0, "L0")).unwrap();
    let d = |id: &str| route.segments.iter().find(|s| s.id == id).unwrap().duration;
    assert!((d("slow") - 9.0 / 1.3).abs() < 0.05, "{}", d("slow"));
    assert!((d("fast") - 9.0 / 2.6).abs() < 0.05, "{}", d("fast"));
}

// Graph files are deterministic, carry a schema fingerprint, and props pass through.

#[test]
fn graph_files_are_deterministic() {
    let v = json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": (0..20).map(|i| json!({"id": format!("s{i}"), "space": format!("s{i}"), "level": "L0",
            "polygon": rect(i as f64 * 4.0, 0.0, i as f64 * 4.0 + 4.0, 4.0), "props": {"n": i}})).collect::<Vec<_>>(),
        "portals": (1..20).map(|i| json!({"id": format!("d{i}"), "segment": [[i as f64 * 4.0, 1.0], [i as f64 * 4.0, 2.0]], "level": "L0"})).collect::<Vec<_>>()
    });
    let dir = std::env::temp_dir();
    let mut bytes = vec![];
    for k in 0..2 {
        let (g, _) = compile(&serde_json::from_value(v.clone()).unwrap());
        let p = dir.join(format!(
            "netweevil_indoor_det_{}_{k}.graph",
            std::process::id()
        ));
        io::save(&g, p.to_str().unwrap()).unwrap();
        bytes.push(std::fs::read(&p).unwrap());
        let r = Router::new(io::load(p.to_str().unwrap()).unwrap());
        let route = go(&r, &walk(), &at(1.0, 2.0, "L0"), &at(79.0, 2.0, "L0")).unwrap();
        let s = serde_json::to_value(&route).unwrap();
        assert_eq!(s["segments"][0]["props"], json!({"n": 0}));
        let _ = std::fs::remove_file(&p);
    }
    assert!(bytes[0] == bytes[1]);
    assert_eq!(&bytes[0][8..16], &io::SCHEMA_FINGERPRINT.to_le_bytes());
}

// A: stairs cost more going up than down (walk), durations unchanged.

#[test]
fn stairs_cost_is_directional() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}, {"id": "L1", "z": 4}],
        "surfaces": [
            {"id": "f0", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 10.0, 10.0)},
            {"id": "f1", "level": "L1", "walls": false, "polygon": rect(0.0, 0.0, 10.0, 10.0)}
        ],
        "verticals": [{"id": "st", "kind": "stairs", "stops": [{"level": "L0", "at": [5, 5]}, {"level": "L1", "at": [5, 5]}]}]
    }));
    let up = go(&r, &walk(), &at(5.0, 5.0, "L0"), &at(5.0, 5.0, "L1")).unwrap();
    let down = go(&r, &walk(), &at(5.0, 5.0, "L1"), &at(5.0, 5.0, "L0")).unwrap();
    let t = 4.0 / 1.3;
    assert!(
        (up.cost - 2.2 * t).abs() < 0.01 && (down.cost - 1.6 * t).abs() < 0.01,
        "{} {}",
        up.cost,
        down.cost
    );
    assert!((up.duration - down.duration).abs() < 1e-6);
    let flat = prof(json!({"kind_factor": {"stairs": 3.0}}));
    let up = go(&r, &flat, &at(5.0, 5.0, "L0"), &at(5.0, 5.0, "L1")).unwrap();
    assert!((up.cost - 3.0 * 4.0 / 1.3).abs() < 0.01, "{}", up.cost);
}

// B: U-turn spikes at nodes are trimmed; edge-to-edge corners are rounded.

#[test]
fn uturn_at_node_is_trimmed() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [{"id": "plaza", "level": "L0", "walls": false, "polygon": rect(0.0, 0.0, 10.0, 10.0)}],
        "edges": [{"id": "e", "line": [[9.5, 5], [9.5, 20]], "level": "L0"}]
    }));
    let route = go(&r, &walk(), &at(9.5, 5.8, "L0"), &at(9.5, 18.0, "L0")).unwrap();
    assert!(
        max_turn(&route.coords) < 2.6,
        "spike left: {:?}",
        route.coords
    );
    assert!((route.length - 12.2).abs() < 0.05, "{}", route.length);
    let total: f64 = route.segments.iter().map(|s| s.duration).sum();
    assert!((total - route.duration).abs() < 1e-6);
}

#[test]
fn edge_corners_are_rounded() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "edges": [
            {"id": "a", "line": [[0, 0], [10, 0]], "level": "L0"},
            {"id": "b", "line": [[10, 0], [10, 10]], "level": "L0"}
        ]
    }));
    let route = go(&r, &walk(), &at(1.0, 0.0, "L0"), &at(10.0, 9.0, "L0")).unwrap();
    let corner = [10.0, 0.0];
    let nearest = route
        .coords
        .iter()
        .map(|p| (p[0] - corner[0]).hypot(p[1] - corner[1]))
        .fold(f64::INFINITY, f64::min);
    let rad = walk().corner_radius;
    assert!(nearest > 0.05 && nearest <= rad + 1e-9, "{nearest}");
    assert!(route.coords.len() > 3);
    assert_eq!(ids(&route), vec!["a", "b"]);
}

/// Two floors joined by a main stair at the far end and an emergency stair near the start.
fn two_stairs(len: f64) -> Value {
    json!({
        "levels": [{"id": "L0", "z": 0}, {"id": "L1", "z": 3.5}],
        "surfaces": [
            {"id": "c0", "level": "L0", "polygon": rect(0.0, 0.0, len, 4.0), "class": "corridor"},
            {"id": "c1", "level": "L1", "polygon": rect(0.0, 0.0, len, 4.0), "class": "corridor"}
        ],
        "verticals": [
            {"id": "main", "kind": "stairs", "tags": ["role:main"],
             "stops": [{"level": "L0", "at": [len - 3.0, 2.0]}, {"level": "L1", "at": [len - 3.0, 2.0]}]},
            {"id": "emergency", "kind": "stairs", "tags": ["role:emergency"],
             "stops": [{"level": "L0", "at": [3.0, 2.0]}, {"level": "L1", "at": [3.0, 2.0]}]}
        ]
    })
}

#[test]
fn emergency_stairs_only_when_much_better() {
    let used = |r: &Route| {
        ids(r)
            .iter()
            .find(|s| **s == "main" || **s == "emergency")
            .map(|s| s.to_string())
    };
    // short corridor: the main stair costs ~50 s extra walking, less than the 120 s penalty
    let r = router(two_stairs(40.0));
    let route = go(&r, &walk(), &at(2.0, 2.0, "L0"), &at(2.0, 2.0, "L1")).unwrap();
    assert_eq!(used(&route).as_deref(), Some("main"));
    // the duration excludes the preference penalty
    assert!(
        route.duration < route.cost,
        "{} {}",
        route.duration,
        route.cost
    );
    let none = prof(json!({"name": "plain", "speed": 1.3}));
    let route = go(&r, &none, &at(2.0, 2.0, "L0"), &at(2.0, 2.0, "L1")).unwrap();
    assert_eq!(used(&route).as_deref(), Some("emergency"));
    // the evacuation profile prefers the emergency stair
    let evac = Profile::builtin("evacuation").unwrap();
    let route = go(&r, &evac, &at(2.0, 2.0, "L0"), &at(2.0, 2.0, "L1")).unwrap();
    assert_eq!(used(&route).as_deref(), Some("emergency"));
    // long corridor: the detour (~300 s) is worse than the penalty, so walk takes the emergency stair
    let r = router(two_stairs(200.0));
    let route = go(&r, &walk(), &at(2.0, 2.0, "L0"), &at(2.0, 2.0, "L1")).unwrap();
    assert_eq!(used(&route).as_deref(), Some("emergency"));
}

#[test]
fn tag_penalty_validated_and_on_portals_edges() {
    assert!(Profile::resolve(r#"{"name":"x","speed":1,"tag_penalty":{"a":-1}}"#).is_err());
    // two rooms joined by a normal door and a service door 2 m apart; the penalty picks the normal one
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "a", "level": "L0", "polygon": rect(0.0, 0.0, 10.0, 10.0)},
            {"id": "b", "level": "L0", "polygon": rect(10.0, 0.0, 20.0, 10.0)}
        ],
        "portals": [
            {"id": "service", "segment": [[10.0, 4.0], [10.0, 5.0]], "level": "L0", "tags": ["role:service"]},
            {"id": "normal", "segment": [[10.0, 8.0], [10.0, 9.0]], "level": "L0"}
        ]
    }));
    let p = prof(json!({"name": "x", "speed": 1.3, "tag_penalty": {"role:service": 60}}));
    let route = go(&r, &p, &at(5.0, 4.5, "L0"), &at(15.0, 4.5, "L0")).unwrap();
    assert!(ids(&route).contains(&"normal"), "{:?}", ids(&route));
    let route = go(
        &r,
        &prof(json!({"name": "y", "speed": 1.3})),
        &at(5.0, 4.5, "L0"),
        &at(15.0, 4.5, "L0"),
    )
    .unwrap();
    assert!(ids(&route).contains(&"service"), "{:?}", ids(&route));
}
