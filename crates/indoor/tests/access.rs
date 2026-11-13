//! Access-controlled routing (`access_groups`): restricted portals, surfaces, verticals, edges,
//! nodes and POIs; granted masks; the profile cache; graph files.

use netweevil_indoor::access::ALL;
use netweevil_indoor::{CompiledProfile, Input, Loc, Profile, Route, Router, compile, io};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]])
}

/// L0: corridor c (open) with rooms R (doors d1 staff-only, d2 public), S (lab-only surface,
/// doors s1 to c and s2 to U), U (door u1 to the side corridor k), W (only door w1,
/// staff-only); L1: corridor c1. Lift (staff-only) at the west end, stairs at the east end.
/// POIs: toilets wc1 (public) and wc2 (staff-only, nearer the room doors).
fn building() -> Value {
    json!({
        "levels": [{"id": "L0", "z": 0}, {"id": "L1", "z": 3.5}],
        "surfaces": [
            {"id": "c", "space": "c", "level": "L0", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)},
            {"id": "k", "space": "k", "level": "L0", "class": "corridor", "walls": false, "cost_factor": 2.0, "polygon": rect(8.0, 3.0, 10.0, 17.0)},
            {"id": "R", "space": "R", "level": "L0", "class": "room", "polygon": rect(10.0, 3.0, 20.0, 10.0)},
            {"id": "S", "space": "S", "level": "L0", "class": "room", "polygon": rect(0.0, 3.0, 8.0, 10.0), "access_groups": ["lab"]},
            {"id": "U", "space": "U", "level": "L0", "class": "room", "polygon": rect(0.0, 10.0, 8.0, 17.0)},
            {"id": "W", "space": "W", "level": "L0", "class": "room", "polygon": rect(22.0, 3.0, 28.0, 10.0)},
            {"id": "c1", "space": "c1", "level": "L1", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)}
        ],
        "portals": [
            {"id": "d1", "kind": "door", "level": "L0", "segment": [[11.0, 3.0], [12.0, 3.0]], "access_groups": ["staff"]},
            {"id": "d2", "kind": "door", "level": "L0", "segment": [[18.0, 3.0], [19.0, 3.0]]},
            {"id": "s1", "kind": "door", "level": "L0", "segment": [[3.5, 3.0], [4.5, 3.0]]},
            {"id": "s2", "kind": "door", "level": "L0", "segment": [[3.5, 10.0], [4.5, 10.0]]},
            {"id": "u1", "kind": "door", "level": "L0", "segment": [[8.0, 13.0], [8.0, 14.0]]},
            {"id": "w1", "kind": "door", "level": "L0", "segment": [[24.0, 3.0], [25.0, 3.0]], "access_groups": ["staff", "cleaning"]}
        ],
        "verticals": [
            {"id": "lift", "kind": "lift", "wait": 5.0, "access_groups": ["staff"],
             "stops": [{"level": "L0", "at": [1.0, 1.5]}, {"level": "L1", "at": [1.0, 1.5]}]},
            {"id": "st", "kind": "stairs",
             "stops": [{"level": "L0", "at": [29.0, 1.5]}, {"level": "L1", "at": [29.0, 1.5]}]}
        ],
        "pois": [
            {"id": "wc1", "category": "toilet", "at": [26.0, 1.5], "level": "L0"},
            {"id": "wc2", "category": "toilet", "at": [13.0, 1.5], "level": "L0", "access_groups": ["staff"]}
        ]
    })
}

fn input(v: Value) -> Input {
    serde_json::from_value(v).unwrap()
}

fn router(v: Value) -> Router {
    Router::new(compile(&input(v)).0)
}

fn pt(x: f64, y: f64, level: &str) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(level.into()),
        ..Default::default()
    }
}

fn walk() -> Profile {
    Profile::builtin("walk").unwrap()
}

fn go(r: &Router, granted: &[&str], a: &Loc, b: &Loc) -> Result<Route, String> {
    let cp = r.profile_access(&walk(), r.graph.access_mask(granted));
    r.route(&mut r.ctx(), &cp, a, b)
}

fn ids(r: &Route) -> Vec<&str> {
    r.segments.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn restricted_door_detour_and_grant() {
    let r = router(building());
    let (a, b) = (pt(11.5, 1.5, "L0"), pt(11.5, 8.0, "L0"));
    let anon = go(&r, &[], &a, &b).unwrap();
    assert!(ids(&anon).contains(&"d2") && !ids(&anon).contains(&"d1"));
    // `profile()` is anonymous too
    let cp = r.profile(&walk());
    let same = r.route(&mut r.ctx(), &cp, &a, &b).unwrap();
    assert_eq!(same.coords, anon.coords);
    for granted in [&["staff"][..], &["*"], &["visitor", "staff"]] {
        let route = go(&r, granted, &a, &b).unwrap();
        assert!(ids(&route).contains(&"d1"), "{granted:?}");
        assert!(route.cost < anon.cost);
    }
    // unknown names are ignored
    let route = go(&r, &["nobody"], &a, &b).unwrap();
    assert!(!ids(&route).contains(&"d1"));
}

#[test]
fn segment_access_on_restricted_door() {
    let r = router(building());
    let route = go(&r, &["staff"], &pt(11.5, 1.5, "L0"), &pt(11.5, 8.0, "L0")).unwrap();
    let door = route.segments.iter().find(|s| s.id == "d1").unwrap();
    assert_eq!(door.access.as_deref(), Some(&["staff".to_string()][..]));
    assert!(
        route
            .segments
            .iter()
            .filter(|s| s.id != "d1")
            .all(|s| s.access.is_none())
    );
    let v = serde_json::to_value(&route).unwrap();
    let segs = v["segments"].as_array().unwrap();
    assert!(segs.iter().any(|s| s["access"] == json!(["staff"])));
    assert!(segs.iter().any(|s| s.get("access").is_none()));
    // walk segments in a restricted surface carry its groups
    let route = go(&r, &["lab"], &pt(4.0, 1.5, "L0"), &pt(4.0, 15.0, "L0")).unwrap();
    assert!(
        route
            .segments
            .iter()
            .any(|s| s.space.as_deref() == Some("S") && s.access == Some(vec!["lab".into()]))
    );
}

#[test]
fn restricted_surface_blocks_and_hides() {
    let r = router(building());
    let (a, b) = (pt(4.0, 1.5, "L0"), pt(4.0, 15.0, "L0"));
    let anon = go(&r, &[], &a, &b).unwrap();
    assert!(!anon.spaces.contains(&"S".to_string()), "{:?}", anon.spaces);
    assert!(anon.spaces.contains(&"k".to_string()));
    let lab = go(&r, &["lab"], &a, &b).unwrap();
    assert!(
        lab.spaces.contains(&"S".to_string()) && lab.cost < anon.cost,
        "{:?} {:?} {} {}",
        lab.spaces,
        ids(&lab),
        lab.cost,
        anon.cost
    );
    // a point inside the restricted room: no route without access, never snapped out of it
    let inside = pt(4.0, 6.0, "L0");
    let e = go(&r, &[], &a, &inside).unwrap_err();
    assert!(e.contains("no walkable surface"), "{e}");
    assert!(go(&r, &["lab"], &a, &inside).is_ok());
    // the space locator: unknown without access
    let sp = Loc {
        space: Some("S".into()),
        ..Default::default()
    };
    assert_eq!(go(&r, &[], &a, &sp).unwrap_err(), "to: unknown space \"S\"");
    assert!(go(&r, &["lab"], &a, &sp).is_ok());
    // a spot in it: same
    let spot = Loc {
        at: Some(vec![6.0, 6.0]),
        space: Some("S".into()),
        ..Default::default()
    };
    assert!(go(&r, &[], &a, &spot).is_err());
    // the restricted door w1: no way into W without access
    let w = pt(25.0, 7.0, "L0");
    assert!(go(&r, &[], &a, &w).is_err());
    assert!(go(&r, &["cleaning"], &a, &w).is_ok());
    // portal / node locators of restricted elements are unknown
    let d = Loc {
        portal: Some("d1".into()),
        ..Default::default()
    };
    assert_eq!(
        go(&r, &[], &a, &d).unwrap_err(),
        "to: unknown portal \"d1\""
    );
    assert!(go(&r, &["staff"], &a, &d).is_ok());
}

#[test]
fn approach_respects_access() {
    let r = router(building());
    let a = pt(4.0, 1.5, "L0");
    let appr = |l: Loc| Loc {
        approach: true,
        ..l
    };
    // a restricted room is never approached from outside
    assert!(go(&r, &[], &a, &appr(pt(4.0, 6.0, "L0"))).is_err());
    let sp = appr(Loc {
        space: Some("S".into()),
        ..Default::default()
    });
    assert!(go(&r, &[], &a, &sp).is_err());
    // nor is a room whose only door is restricted
    let w = appr(pt(25.0, 7.0, "L0"));
    assert!(go(&r, &[], &a, &w).is_err());
    let route = go(&r, &["staff"], &a, &w).unwrap();
    assert!(route.to_approach.is_none() && ids(&route).contains(&"w1"));

    // a doorless room D next to a restricted area X (door x1 to the corridor): the approach
    // point is never inside X without access
    let v = json!({
        "levels": [{"id": "L0", "z": 0}],
        "surfaces": [
            {"id": "c", "space": "c", "level": "L0", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)},
            {"id": "X", "space": "X", "level": "L0", "class": "room", "polygon": rect(0.0, 3.0, 10.0, 6.0), "access_groups": ["lab"]},
            {"id": "D", "space": "D", "level": "L0", "class": "room", "polygon": rect(0.0, 6.0, 10.0, 12.0)}
        ],
        "portals": [{"id": "x1", "kind": "door", "level": "L0", "segment": [[4.0, 3.0], [5.0, 3.0]]}]
    });
    let r = router(v);
    let from = pt(20.0, 1.5, "L0");
    let to = Loc {
        space: Some("D".into()),
        at: Some(vec![5.0, 7.0]),
        approach: true,
        ..Default::default()
    };
    let anon = go(&r, &[], &from, &to).unwrap();
    let ta = anon.to_approach.unwrap();
    assert!(
        ta.at[1] <= 3.0 + 1e-3,
        "approach point {:?} inside X",
        ta.at
    );
    assert!(anon.spaces.iter().all(|s| s == "c"));
    let lab = go(&r, &["lab"], &from, &to).unwrap();
    let ta = lab.to_approach.unwrap();
    assert!(ta.at[1] > 3.0 && ta.at[1] <= 6.0 + 1e-3, "{:?}", ta.at);
}

#[test]
fn restricted_vertical_avoided() {
    let r = router(building());
    let (a, b) = (pt(1.5, 1.5, "L0"), pt(1.5, 1.5, "L1"));
    let anon = go(&r, &[], &a, &b).unwrap();
    assert!(ids(&anon).contains(&"st") && !ids(&anon).contains(&"lift"));
    let staff = go(&r, &["staff"], &a, &b).unwrap();
    assert!(ids(&staff).contains(&"lift"));
    let lift = staff.segments.iter().find(|s| s.id == "lift").unwrap();
    assert_eq!(lift.access, Some(vec!["staff".to_string()]));
    // wheelchair without access: stairs forbidden, lift restricted -> no route
    let cp = r.profile(&Profile::builtin("wheelchair").unwrap());
    assert!(r.route(&mut r.ctx(), &cp, &a, &b).is_err());
}

#[test]
fn restricted_edge_and_node() {
    let v = json!({
        "edges": [
            {"id": "e1", "line": [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]], "from": "a", "to": "b", "access_groups": ["staff"]},
            {"id": "e2", "line": [[0.0, 0.0, 0.0], [5.0, 5.0, 0.0], [10.0, 0.0, 0.0]], "from": "a", "to": "b"},
            {"id": "e3", "line": [[10.0, 0.0, 0.0], [15.0, 0.0, 0.0]], "from": "b", "to": "k"}
        ],
        "nodes": [{"id": "k", "at": [15.0, 0.0, 0.0], "access_groups": ["staff"]}]
    });
    let r = router(v);
    let n = |id: &str| Loc {
        node: Some(id.into()),
        ..Default::default()
    };
    let anon = go(&r, &[], &n("a"), &n("b")).unwrap();
    assert!(ids(&anon).iter().all(|&i| i == "e2"), "{:?}", ids(&anon));
    assert!(
        (anon.length - 2.0 * 50f64.sqrt()).abs() < 1e-3,
        "{}",
        anon.length
    );
    let staff = go(&r, &["staff"], &n("a"), &n("b")).unwrap();
    assert!((staff.length - 10.0).abs() < 1e-3);
    assert_eq!(staff.segments[0].access, Some(vec!["staff".to_string()]));
    // a point on the restricted edge snaps onto the public one
    let on = go(
        &r,
        &[],
        &n("a"),
        &Loc {
            at: Some(vec![5.0, 0.0, 0.0]),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(ids(&on).iter().all(|&i| i == "e2"));
    // restricted node: unknown without access
    assert_eq!(
        go(&r, &[], &n("a"), &n("k")).unwrap_err(),
        "to: unknown node \"k\""
    );
    assert!(go(&r, &["staff"], &n("a"), &n("k")).is_ok());
}

#[test]
fn restricted_pois() {
    let r = router(building());
    let from = pt(12.0, 1.5, "L0");
    let cat = Loc {
        category: Some("toilet".into()),
        ..Default::default()
    };
    let anon = go(&r, &[], &from, &cat).unwrap();
    let hit = anon.to_poi.unwrap();
    assert_eq!((hit.id.as_str(), hit.candidates), ("wc1", 1));
    let staff = go(&r, &["staff"], &from, &cat).unwrap();
    let hit = staff.to_poi.unwrap();
    assert_eq!((hit.id.as_str(), hit.candidates), ("wc2", 2));
    // explicit: the generic error of a POI off the graph
    let wc2 = Loc {
        poi: Some("wc2".into()),
        ..Default::default()
    };
    assert_eq!(
        go(&r, &[], &from, &wc2).unwrap_err(),
        "to: no usable access point (poi not on the graph for this profile)"
    );
    assert!(go(&r, &["staff"], &from, &wc2).is_ok());
    // a list: hidden ones are skipped
    let both = Loc {
        pois: vec!["wc1".into(), "wc2".into()],
        ..Default::default()
    };
    let hit = go(&r, &[], &from, &both).unwrap().to_poi.unwrap();
    assert_eq!((hit.id.as_str(), hit.candidates), ("wc1", 1));
    // only hidden ones in a category: as if there were none
    let mut v = building();
    v["pois"][0]["category"] = json!("kiosk");
    let r2 = router(v);
    assert_eq!(
        go(&r2, &[], &from, &cat).unwrap_err(),
        "to: no poi in category \"toilet\""
    );
    // /check lists hidden POIs as unreachable
    let cp = r.profile(&walk());
    let reach = r.reach(&cp, None, 100).unwrap();
    assert!(reach.unreachable_pois.contains(&"wc2".to_string()));
    let cp = r.profile_access(&walk(), r.graph.access_mask(&["staff"]));
    let reach = r.reach(&cp, None, 100).unwrap();
    assert!(!reach.unreachable_pois.contains(&"wc2".to_string()));
}

#[test]
fn too_many_groups_fail_closed() {
    let mut v = building();
    let nodes: Vec<Value> = (0..70)
        .map(|i| {
            json!({"id": format!("n{i}"), "at": [100.0 + i as f64, 100.0], "level": "L0",
                        "access_groups": [format!("g{i}")]})
        })
        .collect();
    v["nodes"] = json!(nodes);
    // the lift needs a group past the 63rd (verticals compile after nodes)
    v["verticals"][0]["access_groups"] = json!(["g69"]);
    let (g, rep) = compile(&input(v));
    assert_eq!(g.access_groups.len(), 63);
    let w: Vec<_> = rep.warnings.iter().filter(|w| w.kind == "input").collect();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].msg.contains("access groups"));
    assert!(!g.access_groups.contains(&"g69".to_string()));
    let r = Router::new(g);
    let (a, b) = (pt(1.5, 1.5, "L0"), pt(1.5, 1.5, "L1"));
    for granted in [&[][..], &["g69"], &["g5", "g68", "g69", "staff"]] {
        let route = go(&r, granted, &a, &b).unwrap();
        assert!(!ids(&route).contains(&"lift"), "{granted:?}");
    }
    let route = go(&r, &["*"], &a, &b).unwrap();
    let lift = route.segments.iter().find(|s| s.id == "lift").unwrap();
    assert_eq!(lift.access, Some(vec!["@overflow".to_string()]));
    // groups with their own bit still work
    let route = go(&r, &["staff"], &pt(11.5, 1.5, "L0"), &pt(11.5, 8.0, "L0")).unwrap();
    assert!(ids(&route).contains(&"d1"));
}

#[test]
fn graph_file_keeps_access() {
    let (g, _) = compile(&input(building()));
    let path = format!("{}/access.graph", env!("CARGO_TARGET_TMPDIR"));
    io::save(&g, &path).unwrap();
    let h = io::load(&path).unwrap();
    assert_eq!(g.access_groups, h.access_groups);
    assert_eq!(g.access_groups, ["lab", "staff", "cleaning"]);
    assert!(
        g.elems
            .iter()
            .zip(&h.elems)
            .all(|(a, b)| a.access == b.access)
    );
    assert!(
        g.pois
            .iter()
            .zip(&h.pois)
            .all(|(a, b)| a.access == b.access)
    );
    let (r1, r2) = (Router::new(g), Router::new(h));
    let (a, b) = (pt(11.5, 1.5, "L0"), pt(11.5, 8.0, "L0"));
    for granted in [&[][..], &["staff"]] {
        let (x, y) = (
            go(&r1, granted, &a, &b).unwrap(),
            go(&r2, granted, &a, &b).unwrap(),
        );
        assert_eq!(x.coords, y.coords);
    }
}

#[test]
fn no_access_rules_same_profile() {
    let mut v = building();
    for k in ["surfaces", "portals", "verticals", "pois"] {
        for e in v[k].as_array_mut().unwrap() {
            e.as_object_mut().unwrap().remove("access_groups");
        }
    }
    let r = router(v);
    assert!(r.graph.access_groups.is_empty() && !r.graph.has_access_rules());
    let base = r.profile(&walk());
    for m in [0, 1, 0b101, ALL] {
        assert!(Arc::ptr_eq(&base, &r.profile_access(&walk(), m)), "{m}");
    }
    let (a, b) = (pt(11.5, 1.5, "L0"), pt(4.0, 15.0, "L0"));
    let x = r.route(&mut r.ctx(), &base, &a, &b).unwrap();
    let y = go(&r, &["*"], &a, &b).unwrap();
    assert_eq!((x.coords, x.cost), (y.coords, y.cost));
}

/// A synthetic building: `floors` x 4 wings x 2 x 25 rooms with a door each.
fn big(floors: usize) -> Value {
    let (mut levels, mut surfaces, mut portals, mut verticals) = (vec![], vec![], vec![], vec![]);
    let len = 125.0;
    let mut n = 0;
    for f in 0..floors {
        let lv = format!("B:{f}");
        levels.push(json!({"id": lv, "z": f as f64 * 3.5}));
        surfaces.push(
            json!({"id": format!("{lv}:cross"), "level": lv, "class": "corridor", "walls": false,
            "polygon": rect(-4.0, 0.0, 0.0, 64.0)}),
        );
        for w in 0..4 {
            let y = w as f64 * 16.0 + 6.0;
            surfaces.push(
                json!({"id": format!("{lv}:c{w}"), "level": lv, "class": "corridor", "walls": false,
                "polygon": rect(0.0, y, len, y + 2.0)}),
            );
            for k in 0..25 {
                let x = k as f64 * 5.0;
                for (side, (y0, y1)) in [(0, (y - 6.0, y)), (1, (y + 2.0, y + 8.0))] {
                    let id = format!("{lv}:w{w}:r{k}:{side}");
                    surfaces.push(json!({"id": id, "space": id, "level": lv, "class": "room", "polygon": rect(x, y0, x + 5.0, y1)}));
                    let dy = if side == 0 { y } else { y + 2.0 };
                    let mut d = json!({"id": format!("d:{id}"), "kind": "door", "level": lv,
                        "segment": [[x + 2.0, dy], [x + 2.9, dy]]});
                    if n % 20 == 0 {
                        d["access_groups"] = json!(["staff"]);
                    }
                    n += 1;
                    portals.push(d);
                }
            }
        }
    }
    for w in 0..4 {
        let y = w as f64 * 16.0 + 7.0;
        verticals.push(json!({"id": format!("stairs:{w}"), "kind": "stairs",
            "stops": (0..floors).map(|f| json!({"level": format!("B:{f}"), "at": [len - 1.0, y]})).collect::<Vec<_>>()}));
    }
    json!({"levels": levels, "surfaces": surfaces, "portals": portals, "verticals": verticals})
}

#[test]
fn restricting_is_cheap_and_cached() {
    let r = router(big(2));
    let p = walk();
    let t = Instant::now();
    let base = CompiledProfile::new(&r.graph, &p);
    let base_t = t.elapsed();
    let _ = r.graph.has_access_rules(); // the restricted-arc index is built once per graph
    let t = Instant::now();
    let restricted = base.restrict(&r.graph, 0).unwrap();
    let derive_t = t.elapsed();
    assert!(derive_t < base_t, "derive {derive_t:?} vs base {base_t:?}");
    assert!(restricted.factor.iter().filter(|f| f.is_infinite()).count() >= 20);
    assert!(base.restrict(&r.graph, ALL).is_none());
    let mask = r.graph.access_mask(&["staff"]);
    assert!(base.restrict(&r.graph, mask).is_none());
    let (a, b) = (r.profile_access(&p, 0), r.profile_access(&p, 0));
    assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &r.profile(&p)));
    let s = r.profile_access(&p, mask);
    assert!(!Arc::ptr_eq(&a, &s) && Arc::ptr_eq(&s, &r.profile_access(&p, mask)));
    // the anonymous variant equals a profile that forbids the same doors outright
    assert_eq!(a.arc_cost.len(), s.arc_cost.len());
    assert!(
        a.arc_cost
            .iter()
            .zip(&s.arc_cost)
            .all(|(x, y)| x >= y || (x.is_infinite() && y.is_infinite()))
    );
}
