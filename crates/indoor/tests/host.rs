//! The host-facing API: routing modes, costs between one point and many, element and level
//! listing, and input documents built and saved from Rust.

use netweevil_indoor::{
    ElementKind, Input, IntoAnyhow, Loc, Profile, Route, RouteMode, Router, compile,
};
use serde_json::{Value, json};
use std::path::Path;

fn router(v: Value) -> Router {
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

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]])
}

fn mode(m: RouteMode) -> Profile {
    Profile {
        mode: m,
        ..Profile::builtin("walk").unwrap()
    }
}

fn go(r: &Router, p: &Profile, a: &Loc, b: &Loc) -> Result<Route, String> {
    let cp = r.profile(p);
    r.route(&mut r.ctx(), &cp, a, b)
}

fn types(r: &Route) -> Vec<&str> {
    r.segments.iter().map(|s| s.typ).collect()
}

/// Two walled rooms joined by a corridor through doors, and a footway that starts in room A,
/// runs north of the building and ends in room B, with a named node on it. A POI in room B.
fn site() -> Value {
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "A", "level": "L0", "polygon": rect(0.0, 0.0, 10.0, 10.0), "class": "room"},
            {"id": "C", "level": "L0", "polygon": rect(10.0, 0.0, 40.0, 4.0), "class": "corridor"},
            {"id": "B", "level": "L0", "polygon": rect(40.0, 0.0, 50.0, 10.0), "class": "room"}
        ],
        "portals": [
            {"id": "dA", "kind": "door", "segment": [[10.0, 1.0], [10.0, 3.0]], "level": "L0",
             "tags": ["main"], "props": {"name": "west door"}},
            {"id": "dB", "kind": "entrance", "segment": [[40.0, 1.0], [40.0, 3.0]], "level": "L0"}
        ],
        "edges": [
            {"id": "path", "line": [[5.0, 8.0], [5.0, 20.0], [25.0, 20.0], [45.0, 20.0], [45.0, 8.0]],
             "level": "L0", "class": "footway"}
        ],
        "nodes": [{"id": "n_mid", "at": [25.0, 20.0], "level": "L0", "tags": ["junction"]}],
        "pois": [{"id": "desk", "category": "service/desk", "at": [48.0, 8.0], "level": "L0",
                  "tags": ["staffed"], "props": {"floor": 0}}]
    })
}

#[test]
fn mixed_mode_uses_surfaces_and_edges() {
    let r = router(site());
    let p = mode(RouteMode::Mixed);
    let rt = go(&r, &p, &at(2.0, 2.0), &at(48.0, 2.0)).unwrap();
    assert!(types(&rt).contains(&"walk") && types(&rt).contains(&"portal"));
    let rt = go(&r, &p, &at(2.0, 2.0), &at(25.0, 24.0)).unwrap();
    let t = types(&rt);
    assert!(t.contains(&"walk") && t.contains(&"edge"), "{t:?}");
    // The default profile is the mixed mode and serialises without a `mode` key.
    let walk = Profile::builtin("walk").unwrap();
    assert_eq!(walk.mode, RouteMode::Mixed);
    assert!(serde_json::to_value(&walk).unwrap().get("mode").is_none());
}

#[test]
fn edges_only_never_walks_on_surfaces() {
    let r = router(site());
    let p = mode(RouteMode::EdgesOnly);
    let rt = go(&r, &p, &at(5.0, 15.0), &at(45.0, 15.0)).unwrap();
    assert!(types(&rt).iter().all(|&t| t == "edge"), "{:?}", types(&rt));
    // A point in room A, 1 m from the footway's end, snaps onto the footway.
    let rt = go(&r, &p, &at(5.0, 7.0), &at(45.0, 15.0)).unwrap();
    assert!(types(&rt).iter().all(|&t| t == "edge"), "{:?}", types(&rt));
    // A point deep in room A is not near an edge.
    let e = go(&r, &p, &at(2.0, 2.0), &at(45.0, 15.0)).unwrap_err();
    assert!(e.contains("edges_only") && e.starts_with("from:"), "{e}");
    // Space locators need surfaces.
    let space = Loc {
        space: Some("B".into()),
        ..Default::default()
    };
    let e = go(&r, &p, &at(5.0, 15.0), &space).unwrap_err();
    assert!(e.contains("edges_only"), "{e}");
    // The mode string in profile JSON.
    let p: Profile = serde_json::from_value(json!({"mode": "edges_only"})).unwrap();
    assert_eq!(p.mode, RouteMode::EdgesOnly);
}

#[test]
fn surfaces_only_never_travels_along_edges() {
    let r = router(site());
    let p = mode(RouteMode::SurfacesOnly);
    // Both points lie on the footway, 2 m north of the rooms: the mixed mode travels along it,
    // this mode snaps them into the rooms and walks through the corridor.
    let mixed = go(&r, &mode(RouteMode::Mixed), &at(5.0, 12.0), &at(45.0, 12.0)).unwrap();
    assert!(types(&mixed).contains(&"edge"), "{:?}", types(&mixed));
    let rt = go(&r, &p, &at(5.0, 12.0), &at(45.0, 12.0)).unwrap();
    assert!(!types(&rt).contains(&"edge"), "{:?}", types(&rt));
    let ids: Vec<&str> = rt.segments.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"dA") && ids.contains(&"dB"), "{ids:?}");
    // A point by the footway, far from every surface.
    let e = go(&r, &p, &at(25.0, 25.0), &at(45.0, 9.0)).unwrap_err();
    assert!(
        e.contains("surfaces_only") && e.contains("no walkable surface"),
        "{e}"
    );
    // A node that only edges reach.
    let node = Loc {
        node: Some("n_mid".into()),
        ..Default::default()
    };
    let e = go(&r, &p, &node, &at(45.0, 9.0)).unwrap_err();
    assert!(e.contains("n_mid") && e.contains("surfaces_only"), "{e}");
    assert!(go(&r, &mode(RouteMode::Mixed), &node, &at(45.0, 9.0)).is_ok());
}

#[test]
fn costs_to_and_from_match_routes() {
    let r = router(site());
    let p = Profile::builtin("walk").unwrap();
    let cp = r.profile(&p);
    let mut ctx = r.ctx();
    let from = at(2.0, 2.0);
    let poi = Loc {
        poi: Some("desk".into()),
        ..Default::default()
    };
    let portal = Loc {
        portal: Some("dB".into()),
        ..Default::default()
    };
    let nowhere = Loc {
        at: Some(vec![0.0, 0.0]),
        level: Some("L9".into()),
        ..Default::default()
    };
    let targets = [
        at(48.0, 2.0),
        at(25.0, 24.0),
        portal,
        poi,
        nowhere,
        at(3.0, 3.0),
    ];
    let costs = r.costs_to(&mut ctx, &cp, &from, &targets).unwrap();
    assert!(costs[4].is_none());
    for (k, t) in targets.iter().enumerate() {
        let Some(c) = costs[k] else { continue };
        let rt = r.route(&mut ctx, &cp, &from, t).unwrap();
        assert!(
            (c.cost - rt.cost).abs() < 1e-3 * rt.cost.max(1.0),
            "target {k}: {} vs {}",
            c.cost,
            rt.cost
        );
        assert!(
            c.duration <= rt.duration * 1.01 + 0.01 && c.duration >= rt.duration * 0.85,
            "target {k}: duration {} vs {}",
            c.duration,
            rt.duration
        );
        assert!(
            c.length <= rt.length * 1.01 + 0.01 && c.length >= rt.length * 0.85,
            "target {k}: length {} vs {}",
            c.length,
            rt.length
        );
    }
    let back = r.costs_from(&mut ctx, &cp, &targets, &from).unwrap();
    assert!(back[4].is_none());
    for (k, t) in targets.iter().enumerate() {
        let Some(c) = back[k] else { continue };
        let rt = r.route(&mut ctx, &cp, t, &from).unwrap();
        assert!(
            (c.cost - rt.cost).abs() < 1e-3 * rt.cost.max(1.0),
            "source {k}: {} vs {}",
            c.cost,
            rt.cost
        );
    }
    // An unlocatable fixed end is an error.
    assert!(r.costs_to(&mut ctx, &cp, &targets[4], &targets).is_err());
}

#[test]
fn costs_follow_one_way_edges() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "edges": [
            {"id": "fwd", "line": [[0.0, 0.0], [10.0, 0.0]], "level": "L0", "oneway": true},
            {"id": "loop", "line": [[0.0, 0.0], [0.0, 20.0], [10.0, 20.0], [10.0, 0.0]], "level": "L0"}
        ]
    }));
    let cp = r.profile(&Profile::builtin("walk").unwrap());
    let mut ctx = r.ctx();
    let (a, b) = (at(0.5, 0.0), at(9.5, 0.0));
    let ab = r
        .costs_to(&mut ctx, &cp, &a, std::slice::from_ref(&b))
        .unwrap()[0]
        .unwrap();
    let ba = r
        .costs_to(&mut ctx, &cp, &b, std::slice::from_ref(&a))
        .unwrap()[0]
        .unwrap();
    assert!((ab.length - 9.0).abs() < 0.01, "{ab:?}");
    assert!((ba.length - 51.0).abs() < 0.01, "{ba:?}");
    let ba2 = r
        .costs_from(&mut ctx, &cp, std::slice::from_ref(&b), &a)
        .unwrap()[0]
        .unwrap();
    assert!((ba2.cost - ba.cost).abs() < 1e-3, "{ba2:?} vs {ba:?}");
    let rt = r.route(&mut ctx, &cp, &b, &a).unwrap();
    assert!((rt.cost - ba.cost).abs() < 1e-3);
    assert!((rt.duration - ba.duration).abs() < 1e-3);
}

#[test]
fn elements_and_levels() {
    let r = router(site());
    let portals = r.elements(ElementKind::Portal);
    let da = portals.iter().find(|e| e.id == "dA").unwrap();
    assert_eq!(da.subkind.as_deref(), Some("door"));
    assert_eq!(da.level.as_deref(), Some("L0"));
    assert_eq!(da.tags, vec!["main".to_string()]);
    assert_eq!(da.props["name"], "west door");
    assert_eq!(da.points.len(), 2);
    let spaces: Vec<_> = da
        .points
        .iter()
        .filter_map(|p| p.space.as_deref())
        .collect();
    assert!(spaces.contains(&"A") && spaces.contains(&"C"), "{spaces:?}");
    let c = da.at.unwrap();
    assert!(
        (c[0] - 10.0).abs() < 0.2 && (c[1] - 2.0).abs() < 0.2,
        "{c:?}"
    );
    let db = portals.iter().find(|e| e.id == "dB").unwrap();
    assert_eq!(db.subkind.as_deref(), Some("entrance"));

    let nodes = r.elements(ElementKind::Node);
    let n = nodes.iter().find(|e| e.id == "n_mid").unwrap();
    assert_eq!(n.tags, vec!["junction".to_string()]);
    assert_eq!(n.at.map(|p| [p[0], p[1]]), Some([25.0, 20.0]));

    let pois = r.elements(ElementKind::Poi);
    assert_eq!(pois.len(), 1);
    assert_eq!(pois[0].subkind.as_deref(), Some("service/desk"));
    assert_eq!(pois[0].props["floor"], 0);
    assert_eq!(pois[0].points[0].space.as_deref(), Some("B"));

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/two_floors.json");
    let input = netweevil_indoor::parse_input(&std::fs::read_to_string(dir).unwrap()).unwrap();
    let r = Router::new(compile(&input).0);
    let levels = r.levels();
    assert_eq!(levels.len(), 2);
    assert_eq!((levels[1].id.as_str(), levels[1].z), ("L1", 3.5));
    let verticals = r.elements(ElementKind::Vertical);
    let lift = verticals.iter().find(|e| e.id == "lift_1").unwrap();
    assert_eq!(lift.subkind.as_deref(), Some("lift"));
    let mut lv: Vec<_> = lift.points.iter().filter_map(|p| p.level.clone()).collect();
    lv.sort();
    assert_eq!(lv, vec!["L0".to_string(), "L1".to_string()]);
}

#[test]
fn input_round_trips_and_graph_files_load() {
    let input: Input = serde_json::from_value(site()).unwrap();
    let text = serde_json::to_string(&input).unwrap();
    let back: Input = serde_json::from_str(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(&input).unwrap()
    );
    let (g, _) = compile(&back);
    let path = std::env::temp_dir().join(format!(
        "netweevil_indoor_host_{}.graph",
        std::process::id()
    ));
    netweevil_indoor::save_graph(&g, &path).unwrap();
    let r = Router::new(netweevil_indoor::load_graph(&path).unwrap());
    std::fs::remove_file(&path).ok();
    assert_eq!(r.elements(ElementKind::Portal).len(), 2);
    let e = netweevil_indoor::resolve_profile("nope.json").unwrap_err();
    assert!(e.to_string().contains("nope.json"));
    let bad: Result<(), String> = Err("x".into());
    assert_eq!(bad.anyhow().unwrap_err().to_string(), "x");
}
