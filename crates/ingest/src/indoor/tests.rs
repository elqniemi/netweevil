use netweevil_core::LocalFrame;
use netweevil_indoor::{ElementKind, Loc, Profile, Router, compile};
use osmpbfreader::{OsmObj, Tags};

use super::osm::{level_id, osm_indoor_input, parse_levels, parse_osm_xml};
use super::{IndoorSourceOptions, place_input};
use crate::test_util::{osm_node, osm_way};

const ANCHOR: LocalFrame = LocalFrame {
    lon: 6.5665,
    lat: 53.2194,
    rotation_deg: 0.0,
    elevation_m: 0.0,
};

fn node(id: i64, x: f64, y: f64, tags: &[(&str, &str)]) -> OsmObj {
    let [lon, lat] = ANCHOR.to_lonlat(x, y);
    osm_node(
        id,
        (lon * 1e7).round() as i32,
        (lat * 1e7).round() as i32,
        tags,
    )
}

/// A two-storey building: a 20 x 4 m corridor on both levels, a room north
/// of it on each level behind one shared door node, stairs at the west end,
/// a lift at the east end, and the main entrance in the corridor's east wall
/// on level 0.
fn building() -> Vec<OsmObj> {
    let mut o = vec![
        node(1, 0.0, 0.0, &[]),
        node(2, 20.0, 0.0, &[]),
        node(
            3,
            20.0,
            2.0,
            &[("entrance", "main"), ("level", "0"), ("width", "1.6")],
        ),
        node(4, 20.0, 4.0, &[]),
        node(5, 8.0, 4.0, &[]),
        node(6, 4.0, 4.0, &[("door", "hinged")]),
        node(7, 0.0, 4.0, &[]),
        node(8, 8.0, 10.0, &[]),
        node(9, 0.0, 10.0, &[]),
        // Stairs from level 0 at the west to level 1.
        node(20, 1.0, 1.0, &[]),
        node(21, 6.0, 1.0, &[]),
        // Lift.
        node(30, 18.0, 2.0, &[("highway", "elevator"), ("level", "0;1")]),
        // A cafe on level 0.
        node(
            40,
            12.0,
            2.0,
            &[("amenity", "cafe"), ("level", "0"), ("name", "Coffee")],
        ),
    ];
    o.push(osm_way(
        100,
        &[1, 2, 3, 4, 5, 6, 7, 1],
        &[("indoor", "corridor"), ("level", "0;1")],
    ));
    o.push(osm_way(
        101,
        &[7, 6, 5, 8, 9, 7],
        &[
            ("indoor", "room"),
            ("level", "0"),
            ("name", "Lab"),
            ("ref", "0.01"),
        ],
    ));
    o.push(osm_way(
        102,
        &[7, 6, 5, 8, 9, 7],
        &[
            ("indoor", "room"),
            ("level", "1"),
            ("name", "Room 1.14"),
            ("ref", "1.14"),
        ],
    ));
    o.push(osm_way(
        110,
        &[20, 21],
        &[("highway", "steps"), ("level", "0;1"), ("incline", "up")],
    ));
    o
}

fn tags(pairs: &[(&str, &str)]) -> Tags {
    let mut t = Tags::new();
    for (k, v) in pairs {
        t.insert((*k).into(), (*v).into());
    }
    t
}

#[test]
fn levels_parse_lists_ranges_negatives_and_fractions() {
    assert_eq!(parse_levels(&tags(&[("level", "0;1")])), vec![0.0, 1.0]);
    assert_eq!(
        parse_levels(&tags(&[("level", "1-3")])),
        vec![1.0, 2.0, 3.0]
    );
    assert_eq!(parse_levels(&tags(&[("level", "-2--1")])), vec![-2.0, -1.0]);
    assert_eq!(parse_levels(&tags(&[("level", "-1")])), vec![-1.0]);
    assert_eq!(
        parse_levels(&tags(&[("level", "0.5"), ("repeat_on", "2")])),
        vec![0.5, 2.0]
    );
    assert_eq!(level_id(-1.0), "-1");
    assert_eq!(level_id(0.5), "0.5");
}

#[test]
fn osm_building_becomes_a_routable_two_storey_site() {
    let (input, frame, stats) = osm_indoor_input(&building(), Some(ANCHOR), None).unwrap();
    assert_eq!(frame, ANCHOR);
    let ids: Vec<&str> = input.levels.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["0", "1"]);
    assert_eq!(input.levels[1].z, 3.5);
    // Corridor on two levels plus one room on each.
    assert_eq!(input.surfaces.len(), 4);
    assert!(
        input
            .surfaces
            .iter()
            .any(|s| s.id.as_deref() == Some("w100@1") && s.walls)
    );
    assert_eq!(stats.corridors, 1);
    assert_eq!(stats.rooms, 2);
    // The door is on both levels; the entrance on level 0 only.
    let portals: Vec<(&str, &str)> = input
        .portals
        .iter()
        .map(|p| (p.id.as_deref().unwrap(), p.kind.as_deref().unwrap()))
        .collect();
    assert!(portals.contains(&("n6@0", "door")) && portals.contains(&("n6@1", "door")));
    assert!(portals.contains(&("n3", "entrance")));
    let entrance = input
        .portals
        .iter()
        .find(|p| p.id.as_deref() == Some("n3"))
        .unwrap();
    assert_eq!(entrance.width, Some(1.6));
    assert!(entrance.tags.contains(&"entrance".to_string()));
    assert_eq!(entrance.props["osm_id"], "n3");
    // Stairs and a lift.
    let kinds: Vec<&str> = input
        .verticals
        .iter()
        .map(|v| v.kind.as_deref().unwrap())
        .collect();
    assert!(
        kinds.contains(&"stairs") && kinds.contains(&"lift"),
        "{kinds:?}"
    );
    let stairs = input
        .verticals
        .iter()
        .find(|v| v.kind.as_deref() == Some("stairs"))
        .unwrap();
    assert_eq!(stairs.step_free, Some(false));
    assert_eq!(stairs.stops[0].level.as_deref(), Some("0"));
    // Named rooms and the cafe are POIs with name and ref.
    let room = input
        .pois
        .iter()
        .find(|p| p.props["ref"] == "1.14")
        .unwrap();
    assert_eq!(room.props["name"], "Room 1.14");
    assert!(
        input
            .pois
            .iter()
            .any(|p| p.category.as_deref() == Some("amenity/cafe"))
    );

    let (graph, report) = compile(&input);
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    let router = Router::new(graph);
    let entrances: Vec<_> = router
        .elements(ElementKind::Portal)
        .into_iter()
        .filter(|e| e.tags.contains(&"entrance".to_string()))
        .collect();
    assert_eq!(entrances.len(), 1);
    assert_eq!(entrances[0].points.len(), 2);

    let lab = Loc {
        space: Some("w101".into()),
        ..Loc::default()
    };
    let room_114 = Loc {
        poi: Some("w102".into()),
        ..Loc::default()
    };
    let route = |profile: &str| {
        let p = Profile::builtin(profile).unwrap();
        let cp = router.profile(&p);
        router
            .route(&mut router.ctx(), &cp, &lab, &room_114)
            .unwrap()
    };
    let wheelchair = route("wheelchair");
    let verticals: Vec<&str> = wheelchair
        .segments
        .iter()
        .filter(|s| s.typ == "vertical")
        .map(|s| s.kind.as_str())
        .collect();
    assert_eq!(verticals, ["lift"]);
    let walk = route("walk");
    let verticals: Vec<&str> = walk
        .segments
        .iter()
        .filter(|s| s.typ == "vertical")
        .map(|s| s.kind.as_str())
        .collect();
    assert_eq!(verticals, ["stairs"]);
    // From the entrance into the building.
    let from_entrance = Loc {
        portal: Some("n3".into()),
        ..Loc::default()
    };
    let p = Profile::builtin("walk").unwrap();
    let cp = router.profile(&p);
    let r = router
        .route(&mut router.ctx(), &cp, &from_entrance, &lab)
        .unwrap();
    assert!(r.length > 10.0 && r.length < 40.0, "{}", r.length);
}

#[test]
fn bbox_without_features_is_an_error() {
    let err = osm_indoor_input(&building(), Some(ANCHOR), Some([7.0, 50.0, 7.1, 50.1]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no indoor features"), "{err}");
}

#[test]
fn osm_xml_reads_nodes_ways_and_relations() {
    let xml = r#"<?xml version="1.0"?>
<osm version="0.6">
  <node id="1" lat="53.2194" lon="6.5665"><tag k="door" v="yes"/></node>
  <node id="2" lat="53.2195" lon="6.5666"/>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="corridor"/><tag k="level" v="0"/></way>
  <relation id="20"><member type="way" ref="10" role="outer"/><tag k="type" v="multipolygon"/></relation>
</osm>"#;
    let objs = parse_osm_xml(xml.as_bytes()).unwrap();
    assert_eq!(objs.len(), 4);
    let OsmObj::Way(w) = &objs[2] else { panic!() };
    assert_eq!(w.nodes.len(), 2);
    assert_eq!(w.tags.get("level").map(|v| v.as_str()), Some("0"));
}

#[test]
fn wgs84_and_projected_inputs_are_placed_in_local_metres() {
    let doc = serde_json::json!({
        "crs": "EPSG:4326",
        "levels": [{"id": "0", "z": 0.0}],
        "surfaces": [{"id": "a", "level": "0", "polygon": [[[6.5660, 53.2190], [6.5670, 53.2190], [6.5670, 53.2198], [6.5660, 53.2190]]]}]
    });
    let input: netweevil_indoor::Input = serde_json::from_value(doc).unwrap();
    let (input, frame, format, _) = place_input(input, &IndoorSourceOptions::default()).unwrap();
    assert_eq!(format, "input_json_wgs84");
    assert!((frame.lon - 6.5665).abs() < 1e-9 && (frame.lat - 53.2194).abs() < 1e-9);
    let ring = &input.surfaces[0].polygon.as_ref().unwrap()[0];
    // 0.001 degrees of longitude at 53.2 N is about 66.8 m.
    assert!((ring[1][0] - ring[0][0] - 66.8).abs() < 0.3, "{:?}", ring);

    // RD New (EPSG:28992) metres near Groningen.
    let doc = serde_json::json!({
        "levels": [{"id": "0", "z": 0.0}],
        "nodes": [{"id": "n", "at": [233500.0, 582000.0], "level": "0"},
                  {"id": "m", "at": [233510.0, 582000.0], "level": "0"}]
    });
    let input: netweevil_indoor::Input = serde_json::from_value(doc).unwrap();
    let options = IndoorSourceOptions {
        crs: Some("EPSG:28992".into()),
        ..IndoorSourceOptions::default()
    };
    let (input, frame, format, _) = place_input(input, &options).unwrap();
    assert_eq!(format, "input_json_crs");
    assert!(
        (frame.lon - 6.56).abs() < 0.05 && (frame.lat - 53.22).abs() < 0.05,
        "{frame:?}"
    );
    let (a, b) = (
        input.nodes[0].at.as_ref().unwrap(),
        input.nodes[1].at.as_ref().unwrap(),
    );
    let d = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    // RD scale factor differs from 1 by under 0.1 %.
    assert!((d - 10.0).abs() < 0.02, "{d}");

    // Local metres need an anchor.
    let input: netweevil_indoor::Input =
        serde_json::from_value(serde_json::json!({"nodes": [{"at": [1.0, 2.0]}]})).unwrap();
    assert!(place_input(input, &IndoorSourceOptions::default()).is_err());
}
