//! POI locators: `{poi}`, `{pois}`, `{category, tags, exclude}` (multi-target exactness).

use netweevil_indoor::{Input, Loc, Profile, Route, Router, compile};
use serde_json::json;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
    json!([[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]])
}

/// Two floors (corridor + rooms A, B, C), stairs only between them, an outdoor path east.
fn input(extra_pois: Vec<serde_json::Value>) -> Input {
    let mut surfaces = vec![];
    let mut portals = vec![];
    for (lv, sfx) in [("L0", "0"), ("L1", "1")] {
        surfaces.push(
            json!({"id": format!("cor{sfx}"), "level": lv, "class": "corridor", "walls": false,
                             "polygon": rect(0.0, 0.0, 30.0, 4.0)}),
        );
        for (k, name) in ["A", "B", "C"].iter().enumerate() {
            let x0 = 10.0 * k as f64;
            surfaces.push(
                json!({"id": format!("{name}{sfx}"), "level": lv, "class": "room",
                                 "polygon": rect(x0, 4.0, x0 + 10.0, 10.0)}),
            );
            portals.push(json!({"id": format!("d{name}{sfx}"), "level": lv,
                                "segment": [[x0 + 4.0, 4.0], [x0 + 5.0, 4.0]]}));
        }
    }
    let mut pois = vec![
        json!({"id": "wc1", "category": "toilet/men", "at": [2.0, 8.0], "level": "L0",
               "props": {"name": {"en": "Toilet A"}}}),
        json!({"id": "wc2", "category": "toilet/women", "access": [{"space": "B1"}]}),
        json!({"id": "wcA", "category": "toilet/accessible", "tags": ["wheelchair"],
               "access": [{"portal": "dC0"}]}),
        json!({"id": "wcUp", "category": "toilet/accessible", "tags": ["wheelchair"], "at": [25.0, 8.0], "level": "L1"}),
        json!({"id": "cafe", "category": "food/coffee", "at": [60.0, 2.0], "level": "L0"}),
        json!({"id": "bldg", "category": "building", "access": [{"portal": "dA0"}, {"portal": "dC1"}]}),
        json!({"id": "kiosk", "category": "food/coffee", "at": [45.0, 10.0], "level": "L0"}),
        json!({"id": "lost", "category": "food/coffee", "at": [500.0, 500.0], "level": "L0"}),
    ];
    pois.extend(extra_pois);
    serde_json::from_value(json!({
        "levels": [{"id": "L0", "z": 0.0}, {"id": "L1", "z": 4.0}],
        "surfaces": surfaces, "portals": portals,
        "edges": [{"id": "path", "line": [[29.0, 2.0], [60.0, 2.0]], "level": "L0", "class": "footway"}],
        "verticals": [{"id": "st", "kind": "stairs", "stops": [{"level": "L0", "at": [1.0, 2.0]},
                                                              {"level": "L1", "at": [1.0, 2.0]}]}],
        "pois": pois,
    }))
    .unwrap()
}

fn router(extra: Vec<serde_json::Value>) -> (Router, Vec<String>) {
    let (g, rep) = compile(&input(extra));
    let w = rep.warnings.iter().map(|w| w.to_string()).collect();
    (Router::new(g), w)
}

fn go(r: &Router, p: &str, a: &Loc, b: &Loc) -> Result<Route, String> {
    let cp = r.profile(&Profile::builtin(p).unwrap());
    let mut ctx = r.ctx();
    r.route(&mut ctx, &cp, a, b)
}

fn at(x: f64, y: f64, l: &str) -> Loc {
    Loc {
        at: Some(vec![x, y]),
        level: Some(l.into()),
        ..Default::default()
    }
}
fn poi(id: &str) -> Loc {
    Loc {
        poi: Some(id.into()),
        ..Default::default()
    }
}
fn cat(c: &str, tags: &[&str]) -> Loc {
    Loc {
        category: Some(c.into()),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}
fn node(n: &str) -> Loc {
    Loc {
        node: Some(n.into()),
        ..Default::default()
    }
}

#[test]
fn compile_and_warn() {
    let (r, w) = router(vec![]);
    let g = &r.graph;
    assert_eq!(g.pois.len(), 8);
    assert_eq!(g.pois[g.poi_ids["kiosk"] as usize].snaps.len(), 1);
    assert!(g.pois[g.poi_ids["kiosk"] as usize].nodes.is_empty());
    assert_eq!(g.pois[g.poi_ids["bldg"] as usize].nodes.len(), 4); // two portals, two sides each
    assert!(w.iter().any(|w| w.starts_with("poi lost:")), "{w:?}");
    assert!(!w.iter().any(|w| w.starts_with("poi wc")), "{w:?}");
}

#[test]
fn poi_route_ends_at_access_point() {
    let (r, _) = router(vec![]);
    let route = go(&r, "walk", &at(28.0, 2.0, "L0"), &poi("wc1")).unwrap();
    let hit = route.to_poi.as_ref().unwrap();
    assert_eq!(hit.id, "wc1");
    assert_eq!(hit.level.as_deref(), Some("L0"));
    let end = route.coords.last().unwrap();
    assert!(
        (end[0] - 2.0).abs() < 1e-6 && (end[1] - 8.0).abs() < 1e-6,
        "{end:?}"
    );
    assert_eq!(route.segments.last().map(|s| s.id.as_str()), Some("A0"));
    // the same as routing to the access node itself
    let direct = go(&r, "walk", &at(28.0, 2.0, "L0"), &node("wc1@0")).unwrap();
    assert!((direct.cost - route.cost).abs() < 1e-4);
    // space access: ends inside room B1
    let up = go(&r, "walk", &at(28.0, 2.0, "L0"), &poi("wc2")).unwrap();
    assert_eq!(up.segments.last().map(|s| s.id.as_str()), Some("B1"));
    // outdoor POI on the path end
    let c = go(&r, "walk", &at(2.0, 8.0, "L0"), &poi("cafe")).unwrap();
    assert!((c.coords.last().unwrap()[0] - 60.0).abs() < 1e-6);
    assert!(c.to_poi.is_some());
    // off the path: snapped onto it
    let k = go(&r, "walk", &at(2.0, 8.0, "L0"), &poi("kiosk")).unwrap();
    let e = k.coords.last().unwrap();
    assert!(
        (e[0] - 45.0).abs() < 1e-6 && (e[1] - 2.0).abs() < 1e-6,
        "{e:?}"
    );
    let k2 = go(&r, "walk", &at(50.0, 2.0, "L0"), &poi("kiosk")).unwrap();
    assert!(k2.length > 4.9 && k2.length < 5.1, "{}", k2.length);
    let k3 = go(&r, "walk", &poi("kiosk"), &at(50.0, 2.0, "L0")).unwrap();
    assert!((k3.length - 5.0).abs() < 0.1 && k3.from_poi.unwrap().id == "kiosk");
    // as a source
    let s = go(&r, "walk", &poi("cafe"), &at(2.0, 8.0, "L0")).unwrap();
    assert_eq!(s.from_poi.as_ref().unwrap().id, "cafe");
    assert!((s.cost - c.cost).abs() < 0.5);
}

#[test]
fn multi_access_takes_the_best() {
    let (r, _) = router(vec![]);
    let g = &r.graph;
    let nodes = &g.pois[g.poi_ids["bldg"] as usize].nodes;
    for from in [
        at(1.0, 1.0, "L0"),
        at(29.0, 3.0, "L1"),
        at(25.0, 8.0, "L1"),
        at(15.0, 2.0, "L0"),
    ] {
        let best = go(&r, "walk", &from, &poi("bldg")).unwrap();
        let cp = r.profile(&Profile::builtin("walk").unwrap());
        let mut ctx = r.ctx();
        let mut min = f64::INFINITY;
        assert_eq!(nodes.len(), 4);
        for p in ["dA0", "dC1"] {
            let one = Loc {
                portal: Some(p.into()),
                ..Default::default()
            };
            let c = r.route(&mut ctx, &cp, &from, &one).unwrap().cost;
            min = min.min(c);
        }
        assert!((best.cost - min).abs() < 1e-3, "{} vs {min}", best.cost);
    }
}

#[test]
fn nearest_in_category_is_exact() {
    // 30 extra coffee POIs scattered over both floors' rooms and the path
    let mut extra = vec![];
    let mut seed = 7u64;
    let mut rnd = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as f64 / (1u64 << 31) as f64
    };
    for k in 0..30 {
        let lv = if rnd() < 0.5 { "L0" } else { "L1" };
        let (x, y) = (0.5 + 29.0 * rnd(), 4.5 + 5.0 * rnd());
        extra.push(
            json!({"id": format!("c{k}"), "category": "food/coffee", "at": [x, y], "level": lv}),
        );
    }
    let (r, _) = router(extra);
    let ids: Vec<String> = r
        .graph
        .pois
        .iter()
        .filter(|p| p.in_category("food"))
        .map(|p| p.id.clone())
        .collect();
    for k in 0..40 {
        let lv = if k % 2 == 0 { "L0" } else { "L1" };
        let from = at(0.5 + 29.0 * rnd(), 0.5 + 9.0 * rnd(), lv);
        for prof in ["walk", "wheelchair"] {
            let best = go(&r, prof, &from, &cat("food", &[]));
            let mut min = (f64::INFINITY, String::new());
            for id in &ids {
                if let Ok(rt) = go(&r, prof, &from, &poi(id))
                    && rt.cost < min.0
                {
                    min = (rt.cost, id.clone());
                }
            }
            match best {
                Ok(b) => {
                    assert!(
                        (b.cost - min.0).abs() <= 1e-3 * min.0.max(1.0),
                        "{prof}: {} vs {:?}",
                        b.cost,
                        min
                    );
                    let hit = b.to_poi.unwrap();
                    assert_eq!(hit.candidates, ids.len());
                    let own = go(&r, prof, &from, &poi(&hit.id)).unwrap().cost;
                    assert!((own - min.0).abs() <= 1e-3 * min.0.max(1.0));
                }
                Err(e) => assert!(min.0.is_infinite(), "{prof}: {e} but {min:?}"),
            }
        }
    }
}

#[test]
fn tags_profile_and_exclude() {
    let (r, _) = router(vec![]);
    let from = at(15.0, 8.0, "L1");
    // walking: the accessible toilet upstairs is nearest
    let w = go(&r, "walk", &from, &cat("toilet", &["wheelchair"])).unwrap();
    assert_eq!(w.to_poi.unwrap().id, "wcUp");
    // all toilets: wc2 (room B1, the start room) wins
    let any = go(&r, "walk", &from, &cat("toilet", &[])).unwrap();
    assert_eq!(any.to_poi.unwrap().id, "wc2");
    // exclude the winner -> the next one
    let mut l = cat("toilet", &["wheelchair"]);
    l.exclude = vec!["wcUp".into()];
    assert_eq!(go(&r, "walk", &from, &l).unwrap().to_poi.unwrap().id, "wcA");
    // wheelchair from the ground floor: upstairs is only reachable by stairs
    let g0 = go(
        &r,
        "wheelchair",
        &at(28.0, 2.0, "L0"),
        &cat("toilet", &["wheelchair"]),
    )
    .unwrap();
    assert_eq!(g0.to_poi.unwrap().id, "wcA");
    assert!(go(&r, "wheelchair", &at(28.0, 2.0, "L0"), &poi("wcUp")).is_err());
    // prefix matches whole path segments only
    assert!(go(&r, "walk", &from, &cat("toil", &[])).is_err());
    assert!(
        go(&r, "walk", &from, &poi("nope"))
            .unwrap_err()
            .contains("unknown poi")
    );
    // `pois` list: best of the given ids
    let l = Loc {
        pois: vec!["wc1".into(), "cafe".into()],
        ..Default::default()
    };
    assert_eq!(
        go(&r, "walk", &at(3.0, 2.0, "L0"), &l)
            .unwrap()
            .to_poi
            .unwrap()
            .id,
        "wc1"
    );
}

#[test]
fn reach_lists_unreachable_pois() {
    let (r, _) = router(vec![]);
    let cp = r.profile(&Profile::builtin("wheelchair").unwrap());
    let reach = r.reach(&cp, Some("dA0"), 100).unwrap();
    let mut u = reach.unreachable_pois.clone();
    u.sort();
    assert_eq!(u, vec!["lost", "wc2", "wcUp"]);
    assert_eq!(reach.counts.pois, 3);
}

#[test]
fn locator_parse() {
    let l = Loc::parse("category=toilet|tag=wheelchair").unwrap();
    assert_eq!(l.category.as_deref(), Some("toilet"));
    assert_eq!(l.tags, vec!["wheelchair"]);
    assert_eq!(Loc::parse("poi=wc1").unwrap().poi.as_deref(), Some("wc1"));
}
