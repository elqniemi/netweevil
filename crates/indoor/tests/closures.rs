//! Live closures (`closure.rs`): closed and slowed doors, lifts and spaces, polygon areas,
//! per-query `avoid`, the closures a route reports, the overlay cache and `POST /route`.

use netweevil_indoor::{Closure, Input, Loc, Profile, Route, Router, compile};
use serde_json::{Value, json};
use std::sync::Arc;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]])
}

/// L0: open corridor c (0..30 x 0..3) with room R above it (10..20 x 3..10), doors d1 (x 11)
/// and d2 (x 19) from c into R. L1: corridor c1. A lift at the west end, stairs at the east.
fn building() -> Router {
    let v = json!({
        "levels": [{"id": "L0", "z": 0}, {"id": "L1", "z": 3.5}],
        "surfaces": [
            {"id": "c", "space": "c", "level": "L0", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)},
            {"id": "R", "space": "R", "level": "L0", "class": "room", "polygon": rect(10.0, 3.0, 20.0, 10.0)},
            {"id": "c1", "space": "c1", "level": "L1", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)}
        ],
        "portals": [
            {"id": "d1", "kind": "door", "level": "L0", "segment": [[11.0, 3.0], [12.0, 3.0]]},
            {"id": "d2", "kind": "door", "level": "L0", "segment": [[18.0, 3.0], [19.0, 3.0]]}
        ],
        "verticals": [
            {"id": "lift", "kind": "lift", "wait": 5.0, "stops": [{"level": "L0", "at": [1.0, 1.5]}, {"level": "L1", "at": [1.0, 1.5]}]},
            {"id": "st", "kind": "stairs", "stops": [{"level": "L0", "at": [29.0, 1.5]}, {"level": "L1", "at": [29.0, 1.5]}]}
        ]
    });
    let input: Input = serde_json::from_value(v).unwrap();
    Router::new(compile(&input).0)
}

fn pt(x: f64, y: f64, level: &str) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(level.into()),
        ..Default::default()
    }
}

fn closure(v: Value) -> Closure {
    serde_json::from_value(v).unwrap()
}

fn go(r: &Router, a: &Loc, b: &Loc, list: &[Closure]) -> Result<Route, String> {
    let cp = r.profile(&Profile::builtin("walk").unwrap());
    let mut ctx = r.ctx();
    if list.is_empty() {
        r.route(&mut ctx, &cp, a, b)
    } else {
        r.route_closed(&mut ctx, &cp, a, b, &r.overlay(list))
    }
}

fn ids(r: &Route) -> Vec<&str> {
    r.segments.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn closed_door_detours_and_is_reported() {
    let r = building();
    let (a, b) = (pt(11.5, 1.5, "L0"), pt(15.0, 8.0, "L0"));
    let free = go(&r, &a, &b, &[]).unwrap();
    assert!(ids(&free).contains(&"d1"));
    assert!(free.closures.is_empty());
    let c = closure(json!({"id": "c1", "reason": "Cleaning", "elements": ["d1"]}));
    let x = go(&r, &a, &b, &[c]).unwrap();
    assert!(!ids(&x).contains(&"d1"), "{:?}", ids(&x));
    assert!(ids(&x).contains(&"d2"));
    assert!(x.cost > free.cost);
    assert_eq!(x.closures.len(), 1);
    let h = &x.closures[0];
    assert_eq!(h.id.as_deref(), Some("c1"));
    assert_eq!(h.reason.as_deref(), Some("Cleaning"));
    assert_eq!(h.effect, "detour");
    assert_eq!(h.level.as_deref(), Some("L0"));
    assert!(
        (h.at[0] - 11.5).abs() < 0.6 && (h.at[1] - 3.0).abs() < 0.6,
        "{:?}",
        h.at
    );
}

#[test]
fn closures_elsewhere_change_nothing() {
    let r = building();
    let (a, b) = (pt(11.5, 1.5, "L0"), pt(15.0, 8.0, "L0"));
    let free = go(&r, &a, &b, &[]).unwrap();
    let x = go(
        &r,
        &a,
        &b,
        &[closure(json!({"id": "far", "elements": ["lift"]}))],
    )
    .unwrap();
    assert_eq!(free.cost, x.cost);
    assert_eq!(free.coords, x.coords);
    assert!(x.closures.is_empty());
}

#[test]
fn waits_add_time_or_detour() {
    let r = building();
    let (a, b) = (pt(11.5, 1.5, "L0"), pt(15.0, 8.0, "L0"));
    let free = go(&r, &a, &b, &[]).unwrap();
    // A short wait: the route keeps the door and its duration grows by the wait.
    let x = go(
        &r,
        &a,
        &b,
        &[closure(json!({"id": "w", "elements": ["d1"], "wait": 2.0}))],
    )
    .unwrap();
    assert!(ids(&x).contains(&"d1"));
    assert!(
        (x.duration - free.duration - 2.0).abs() < 0.05,
        "{} vs {}",
        x.duration,
        free.duration
    );
    assert_eq!(x.closures[0].effect, "wait");
    assert_eq!(x.closures[0].wait, Some(2.0));
    // A long wait: the other door is quicker.
    let y = go(
        &r,
        &a,
        &b,
        &[closure(
            json!({"id": "w", "elements": ["d1"], "wait": 300.0}),
        )],
    )
    .unwrap();
    assert!(ids(&y).contains(&"d2"));
    assert_eq!(y.closures[0].effect, "detour");
}

#[test]
fn closed_lift_takes_the_stairs() {
    let r = building();
    let (a, b) = (pt(2.0, 1.5, "L0"), pt(2.0, 1.5, "L1"));
    assert!(ids(&go(&r, &a, &b, &[]).unwrap()).contains(&"lift"));
    let x = go(
        &r,
        &a,
        &b,
        &[closure(json!({"id": "lift-off", "elements": ["lift"]}))],
    )
    .unwrap();
    assert!(ids(&x).contains(&"st"));
    assert_eq!(x.closures[0].id.as_deref(), Some("lift-off"));
}

#[test]
fn polygon_blocks_the_corridor() {
    let r = building();
    let (a, b) = (pt(2.0, 1.5, "L0"), pt(28.0, 1.5, "L0"));
    let free = go(&r, &a, &b, &[]).unwrap();
    assert!(!ids(&free).contains(&"d1"));
    let block = closure(
        json!({"id": "spill", "polygon": [[14.0, -1.0], [16.0, -1.0], [16.0, 3.0], [14.0, 3.0]], "level": "L0"}),
    );
    let x = go(&r, &a, &b, &[block]).unwrap();
    assert!(
        ids(&x).contains(&"d1") && ids(&x).contains(&"d2"),
        "{:?}",
        ids(&x)
    );
    assert!(
        x.coords
            .iter()
            .all(|p| !(p[0] > 14.0 && p[0] < 16.0 && p[1] < 3.0))
    );
    assert_eq!(x.closures[0].id.as_deref(), Some("spill"));
    assert_eq!(x.closures[0].at, [15.0, 1.0, 0.0]);
    // A route from a point in the area walks out of it.
    let inside = closure(
        json!({"polygon": [[14.0, -1.0], [16.0, -1.0], [16.0, 3.0], [14.0, 3.0]], "level": "L0"}),
    );
    let out = go(&r, &pt(15.0, 1.5, "L0"), &b, &[inside]).unwrap();
    assert!(out.length < 14.0, "{}", out.length);
    // On another floor it changes nothing.
    let other = closure(
        json!({"polygon": [[14.0, -1.0], [16.0, -1.0], [16.0, 3.0], [14.0, 3.0]], "level": "L1"}),
    );
    assert_eq!(go(&r, &a, &b, &[other]).unwrap().cost, free.cost);
}

#[test]
fn closed_space_and_area_leave_no_way() {
    let r = building();
    let (a, b) = (pt(2.0, 1.5, "L0"), pt(28.0, 1.5, "L0"));
    // On every floor: the corridor upstairs is cut too.
    let block = closure(json!({"polygon": [[14.0, -1.0], [16.0, -1.0], [16.0, 3.0], [14.0, 3.0]]}));
    let room = closure(json!({"id": "R-closed", "spaces": ["R"]}));
    let e = go(&r, &a, &b, &[block, room.clone()]).unwrap_err();
    assert!(e.contains("closed"), "{e}");
    // A closed room is not walked through, but a route into it still gets there.
    assert!(go(&r, &a, &pt(15.0, 8.0, "L0"), &[room]).is_ok());
}

#[test]
fn unknown_ids_warn_and_overlays_are_cached() {
    let r = building();
    let list = [closure(json!({"id": "x", "elements": ["nope"]}))];
    let x = go(&r, &pt(2.0, 1.5, "L0"), &pt(28.0, 1.5, "L0"), &list).unwrap();
    assert!(
        x.warnings.iter().any(|w| w.contains("nope")),
        "{:?}",
        x.warnings
    );
    assert!(Arc::ptr_eq(&r.overlay(&list), &r.overlay(&list)));
}
