//! Bike mode: ride outdoors, walk where the bike may not go (`on_foot`), never cross a building
//! on foot mid-ride (`leave_tags`).

use netweevil_indoor::profile::OnFoot;
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

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]])
}

fn bike() -> Profile {
    Profile::builtin("bike").unwrap()
}

fn go(r: &Router, p: &Profile, a: &Loc, b: &Loc) -> Result<Route, String> {
    let cp = r.profile(p);
    let mut ctx = r.ctx();
    r.route(&mut ctx, &cp, a, b)
}

fn modes(r: &Route) -> Vec<&str> {
    r.legs.iter().map(|l| l.mode).collect()
}

/// A plaza (outdoor) with a hall (indoor) east of it behind a door, and three parkings: one
/// far away, one 10 m from the door and a staff one right at the door.
fn campus() -> serde_json::Value {
    json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "plaza", "level": "L0", "class": "plaza", "walls": false, "tags": ["outdoor"],
             "polygon": rect(0.0, 0.0, 100.0, 40.0)},
            {"id": "hall", "space": "hall", "level": "L0", "class": "corridor", "tags": ["indoor"],
             "polygon": rect(100.0, 10.0, 120.0, 30.0)}
        ],
        "portals": [{"id": "door", "kind": "door", "segment": [[100, 18], [100, 22]], "level": "L0", "width": 1.2}],
        "pois": [
            {"id": "p_far", "category": "bicycle_parking", "access": [{"at": [10.0, 35.0], "level": "L0"}]},
            {"id": "p_near", "category": "bicycle_parking", "access": [{"at": [90.0, 20.0], "level": "L0"}]},
            {"id": "p_staff", "category": "bicycle_parking/covered", "access_groups": ["staff"],
             "access": [{"at": [99.0, 20.0], "level": "L0"}]}
        ]
    })
}

#[test]
fn rides_outside_walks_inside_with_modes_and_legs() {
    let r = router(campus());
    let route = go(&r, &bike(), &at(5.0, 5.0), &at(115.0, 20.0)).unwrap();
    assert_eq!(modes(&route), vec!["ride", "walk"]);
    for s in &route.segments {
        let inside = s.space.as_deref() == Some("hall") || s.typ == "portal";
        assert_eq!(
            s.mode,
            Some(if inside { "walk" } else { "ride" }),
            "{} {}",
            s.id,
            s.typ
        );
    }
    let (ride, walk) = (&route.legs[0], &route.legs[1]);
    assert!(
        (ride.length / ride.duration - 4.5).abs() < 0.3,
        "ride {ride:?}"
    );
    // the door costs 2 s on foot
    assert!(
        (walk.length / (walk.duration - 2.0) - 1.3).abs() < 0.15,
        "walk {walk:?}"
    );
    assert_eq!(ride.to_idx, walk.from_idx);
    assert_eq!(walk.to_idx, route.coords.len() - 1);
    // walk profiles carry no modes
    let w = go(
        &r,
        &Profile::builtin("walk").unwrap(),
        &at(5.0, 5.0),
        &at(115.0, 20.0),
    )
    .unwrap();
    assert!(w.legs.is_empty() && w.segments.iter().all(|s| s.mode.is_none()));
}

/// A profile that forbids everything indoor has no bike route that starts or ends in a
/// building. With `on_foot` it walks there.
#[test]
fn profile_forbidding_indoor_walks_with_on_foot() {
    let r = router(campus());
    let mut old = bike();
    old.on_foot = None;
    old.leave_tags.clear();
    old.class_factor.insert("corridor".into(), None);
    assert!(go(&r, &old, &at(5.0, 5.0), &at(115.0, 20.0)).is_err());
    let mut fixed = old.clone();
    fixed.on_foot = Some(OnFoot::Name("walk".into()));
    let route = go(&r, &fixed, &at(5.0, 5.0), &at(115.0, 20.0)).unwrap();
    assert_eq!(modes(&route), vec!["ride", "walk"]);
    // an on-foot profile given as an object
    let mut obj = old.clone();
    obj.on_foot = Some(OnFoot::Profile(Box::new(Profile {
        speed: 1.0,
        ..Profile::builtin("walk").unwrap()
    })));
    let slow = go(&r, &obj, &at(5.0, 5.0), &at(115.0, 20.0)).unwrap();
    assert!(slow.legs[1].duration > route.legs[1].duration * 1.2);
    // profiles round-trip through JSON with on_foot
    let v = serde_json::to_value(&fixed).unwrap();
    assert_eq!(v["on_foot"], json!("walk"));
    let back: Profile = serde_json::from_value(v).unwrap();
    assert!(back.foot().unwrap().is_some());
    let bad: Profile = serde_json::from_value(json!({"on_foot": "nope"})).unwrap();
    assert!(bad.validate().is_err());
}

/// West and east plazas joined by a long way round in the north, with a building between
/// them. Walking goes through the building; the bike rides round, since walking through means
/// leaving the bike behind. Starting inside the building is fine.
#[test]
fn never_crosses_a_building_on_foot_mid_ride() {
    let r = router(json!({
        "levels": [{"id": "L0", "z": 0.0}],
        "surfaces": [
            {"id": "west", "level": "L0", "class": "plaza", "walls": false, "tags": ["outdoor"], "polygon": rect(0.0, 0.0, 40.0, 210.0)},
            {"id": "east", "level": "L0", "class": "plaza", "walls": false, "tags": ["outdoor"], "polygon": rect(60.0, 0.0, 100.0, 210.0)},
            {"id": "north", "level": "L0", "class": "plaza", "walls": false, "tags": ["outdoor"], "polygon": rect(40.0, 200.0, 60.0, 210.0)},
            {"id": "bldg", "space": "bldg", "level": "L0", "class": "corridor", "tags": ["indoor"], "polygon": rect(40.0, 0.0, 60.0, 20.0)}
        ],
        "portals": [
            {"id": "dw", "kind": "door", "segment": [[40, 8], [40, 12]], "level": "L0", "width": 1.2},
            {"id": "de", "kind": "door", "segment": [[60, 8], [60, 12]], "level": "L0", "width": 1.2}
        ]
    }));
    let (a, b) = (at(30.0, 10.0), at(70.0, 10.0));
    let walk = go(&r, &Profile::builtin("walk").unwrap(), &a, &b).unwrap();
    assert!(walk.spaces.contains(&"bldg".to_string()) && walk.length < 60.0);
    let ride = go(&r, &bike(), &a, &b).unwrap();
    assert!(
        !ride.spaces.contains(&"bldg".to_string()),
        "{:?}",
        ride.spaces
    );
    assert!(ride.length > 350.0 && modes(&ride) == vec!["ride"]);
    // without leave_tags the bike is pushed through
    let mut push = bike();
    push.leave_tags.clear();
    let through = go(&r, &push, &a, &b).unwrap();
    assert!(through.spaces.contains(&"bldg".to_string()));
    assert_eq!(modes(&through), vec!["ride", "walk", "ride"]);
    // from inside the building: walk out, then ride
    let out = go(&r, &bike(), &at(50.0, 10.0), &b).unwrap();
    assert_eq!(modes(&out), vec!["walk", "ride"]);
    assert!(out.length < 30.0);
}
