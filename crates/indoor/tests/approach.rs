//! `approach` locators: rooms without doors are reached from the nearest walkable point.

use netweevil_indoor::geom::P3;
use netweevil_indoor::{Input, Loc, Profile, Route, Router, compile};
use serde_json::{Value, json};

fn router(v: Value) -> Router {
    let input: Input = serde_json::from_value(v).unwrap();
    Router::new(compile(&input).0)
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1]]])
}

fn space(s: &str, approach: bool) -> Loc {
    Loc {
        space: Some(s.into()),
        approach,
        ..Default::default()
    }
}

fn point(x: f64, y: f64, level: &str, approach: bool) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(level.into()),
        approach,
        ..Default::default()
    }
}

fn go(r: &Router, p: &str, a: &Loc, b: &Loc) -> Result<Route, String> {
    let cp = r.profile(&Profile::builtin(p).unwrap());
    let mut ctx = r.ctx();
    r.route(&mut ctx, &cp, a, b)
}

fn close(a: P3, b: P3) -> bool {
    (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
}

/// Two floors: an open corridor each, doorless rooms r1, r2 (L0) and r4 (L1), room r3 with a
/// door, and stairs at the east end. `stairs: false` leaves the corridors with no way out.
fn floors(stairs: bool) -> Value {
    let mut v = json!({
        "levels": [{"id": "L0", "z": 0}, {"id": "L1", "z": 3.5}],
        "surfaces": [
            {"id": "c0", "space": "c0", "level": "L0", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)},
            {"id": "r1", "space": "r1", "level": "L0", "class": "room", "polygon": rect(2.0, 3.0, 8.0, 8.0)},
            {"id": "r2", "space": "r2", "level": "L0", "class": "room", "polygon": rect(20.0, 3.0, 26.0, 8.0)},
            {"id": "r3", "space": "r3", "level": "L0", "class": "room", "polygon": rect(10.0, 3.0, 16.0, 8.0)},
            {"id": "c1", "space": "c1", "level": "L1", "class": "corridor", "walls": false, "polygon": rect(0.0, 0.0, 30.0, 3.0)},
            {"id": "r4", "space": "r4", "level": "L1", "class": "room", "polygon": rect(2.0, 3.0, 8.0, 8.0)}
        ],
        "portals": [{"id": "d3", "kind": "door", "segment": [[12.5, 3.0], [13.5, 3.0]], "level": "L0"}]
    });
    if stairs {
        v["verticals"] = json!([{"id": "st", "kind": "stairs",
            "stops": [{"level": "L0", "at": [28.0, 1.5]}, {"level": "L1", "at": [28.0, 1.5]}]}]);
    }
    v
}

#[test]
fn doorless_room_needs_approach() {
    let r = router(floors(true));
    assert!(go(&r, "walk", &space("r1", false), &space("r2", false)).is_err());
    let route = go(&r, "walk", &space("r1", true), &space("r2", true)).unwrap();
    let (fa, ta) = (
        route.from_approach.clone().unwrap(),
        route.to_approach.clone().unwrap(),
    );
    // the route runs along the corridor, from the wall of r1 to the wall of r2
    assert!(close(route.coords[0], fa.at) && close(*route.coords.last().unwrap(), ta.at));
    assert!(
        (fa.at[1] - 3.0).abs() < 0.01 && (2.0..=8.0).contains(&fa.at[0]),
        "{:?}",
        fa.at
    );
    assert!(
        (ta.at[1] - 3.0).abs() < 0.01 && (20.0..=26.0).contains(&ta.at[0]),
        "{:?}",
        ta.at
    );
    assert_eq!(ta.space.as_deref(), Some("r2"));
    assert!(ta.target[1] > 3.0 && (20.0..=26.0).contains(&ta.target[0]));
    let d = ((ta.at[0] - ta.target[0]).powi(2) + (ta.at[1] - ta.target[1]).powi(2)).sqrt();
    assert!((ta.length - d).abs() < 1e-9 && ta.duration > 0.0);
    assert!(route.spaces.iter().all(|s| s == "c0"), "{:?}", route.spaces);
}

#[test]
fn room_with_a_door_is_entered() {
    let r = router(floors(true));
    let route = go(&r, "walk", &point(1.0, 1.5, "L0", true), &space("r3", true)).unwrap();
    assert!(route.from_approach.is_none() && route.to_approach.is_none());
    assert_eq!(route.spaces.last().map(String::as_str), Some("r3"));
}

#[test]
fn approach_across_floors_and_profiles() {
    let r = router(floors(true));
    for p in ["walk", "bike", "fastest"] {
        let route = go(&r, p, &space("r1", true), &space("r4", true)).unwrap();
        assert!(route.segments.iter().any(|s| s.id == "st"), "{p}");
        let ta = route.to_approach.unwrap();
        assert_eq!(ta.level.as_deref(), Some("L1"));
        assert!((ta.at[1] - 3.0).abs() < 0.01);
    }
    // the same room on both ends is a walk inside it
    let route = go(&r, "walk", &point(3.0, 4.0, "L0", true), &space("r1", true)).unwrap();
    assert!(route.from_approach.is_none() && route.to_approach.is_none());
}

#[test]
fn corridor_without_a_way_out() {
    // no stairs, no doors to anything that leads on: both rooms meet in the corridor
    let r = router(floors(false));
    let route = go(&r, "walk", &space("r1", true), &space("r2", true)).unwrap();
    assert!(route.from_approach.is_some() && route.to_approach.is_some());
    assert!(route.spaces.iter().all(|s| s == "c0"));
    // from a point in the corridor
    let route = go(
        &r,
        "wheelchair",
        &point(15.0, 1.5, "L0", true),
        &space("r2", true),
    )
    .unwrap();
    assert!(route.from_approach.is_none() && route.to_approach.is_some());
}

#[test]
fn spot_and_unknown_space() {
    let r = router(floors(true));
    // a spot in a doorless room: the arc ends at the spot
    let spot = Loc {
        at: Some(vec![25.0, 7.0]),
        space: Some("r2".into()),
        approach: true,
        ..Default::default()
    };
    let route = go(&r, "walk", &point(1.0, 1.5, "L0", true), &spot).unwrap();
    let ta = route.to_approach.unwrap();
    assert!(close(ta.target, [25.0, 7.0, 0.0]), "{:?}", ta.target);
    // a room the graph leaves out: its point, from the nearest walkable point
    let gone = Loc {
        at: Some(vec![15.0, 12.0]),
        level: Some("L0".into()),
        space: Some("r9".into()),
        approach: true,
        ..Default::default()
    };
    let route = go(&r, "walk", &point(1.0, 1.5, "L0", true), &gone).unwrap();
    let ta = route.to_approach.unwrap();
    assert!(close(ta.target, [15.0, 12.0, 0.0]) && ta.space.as_deref() == Some("r9"));
    assert!(ta.length > 3.0);
    let plain = Loc {
        approach: false,
        ..gone
    };
    assert!(go(&r, "walk", &point(1.0, 1.5, "L0", false), &plain).is_err());
}

#[test]
fn point_in_a_cut_off_scrap_of_a_room_starts_in_the_room() {
    // a smaller doorless space laid across r3 cuts off a 0.6 m strip along its north wall
    let mut v = floors(true);
    v["surfaces"].as_array_mut().unwrap().push(json!(
        {"id": "shelf", "space": "shelf", "level": "L0", "class": "room", "polygon": rect(9.0, 6.6, 17.0, 7.4)}
    ));
    let r = router(v);
    let route = go(
        &r,
        "walk",
        &point(13.0, 7.7, "L0", false),
        &point(1.0, 1.5, "L0", false),
    )
    .unwrap();
    assert_eq!(
        route.spaces.first().map(String::as_str),
        Some("r3"),
        "{:?}",
        route.spaces
    );
    assert!(
        route.coords[0][1] < 6.6 && route.coords[0][1] > 5.0,
        "{:?}",
        route.coords[0]
    );
    // a room closed as a whole is still nowhere without `approach`
    assert!(
        go(
            &r,
            "walk",
            &point(5.0, 6.0, "L0", false),
            &point(1.0, 1.5, "L0", false)
        )
        .is_err()
    );
}
