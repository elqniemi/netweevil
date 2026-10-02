use super::*;
use crate::test_util::TempDir;
use std::io::Write;

const CHANGE: &str = r#"<?xml version='1.0' encoding='UTF-8'?>
<osmChange version="0.6" generator="test">
  <create>
    <node id="5" version="1" lat="53.2194120" lon="6.5665018">
      <tag k="highway" v="traffic_signals"/>
    </node>
    <way id="20" version="1">
      <nd ref="1"/>
      <nd ref="5"/>
      <tag k="highway" v="residential"/>
      <tag k="name" v="Grote &amp; Kleine &#10;Markt"/>
    </way>
  </create>
  <modify>
    <node id="1" version="3" lat="-0.5" lon="179.99999995"/>
    <relation id="30" version="2">
      <member type="way" ref="20" role="from"/>
      <member type="node" ref="5" role="via"/>
      <member type="way" ref="21" role="to"/>
      <tag k="type" v="restriction"/>
      <tag k="restriction" v="no_left_turn"/>
    </relation>
  </modify>
  <delete>
    <way id="21" version="4"/>
    <node id="2" version="2" visible="false"/>
  </delete>
</osmChange>
"#;

#[test]
fn parses_every_action_and_object_kind() {
    let changes = parse_osm_change(CHANGE.as_bytes()).expect("change parses");
    let summary = changes
        .iter()
        .map(|change| (change.action, change.version, change.object.id()))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            (ChangeAction::Create, 1, OsmId::Node(NodeId(5))),
            (ChangeAction::Create, 1, OsmId::Way(WayId(20))),
            (ChangeAction::Modify, 3, OsmId::Node(NodeId(1))),
            (ChangeAction::Modify, 2, OsmId::Relation(RelationId(30))),
            (ChangeAction::Delete, 4, OsmId::Way(WayId(21))),
            (ChangeAction::Delete, 2, OsmId::Node(NodeId(2))),
        ]
    );

    let OsmObj::Node(signal) = &changes[0].object else {
        panic!("first change is a node");
    };
    assert_eq!(
        (signal.decimicro_lat, signal.decimicro_lon),
        (532_194_120, 65_665_018)
    );
    assert!(signal.tags.contains("highway", "traffic_signals"));
    let OsmObj::Way(way) = &changes[1].object else {
        panic!("second change is a way");
    };
    assert_eq!(way.nodes, vec![NodeId(1), NodeId(5)]);
    assert_eq!(
        way.tags.get("name").map(|name| name.as_str()),
        Some("Grote & Kleine \nMarkt")
    );
    let OsmObj::Node(moved) = &changes[2].object else {
        panic!("third change is a node");
    };
    // Digits beyond the seventh decimal round half away from zero.
    assert_eq!(
        (moved.decimicro_lat, moved.decimicro_lon),
        (-5_000_000, 1_800_000_000)
    );
    let OsmObj::Relation(relation) = &changes[3].object else {
        panic!("fourth change is a relation");
    };
    assert_eq!(
        relation
            .refs
            .iter()
            .map(|member| (member.member, member.role.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (OsmId::Way(WayId(20)), "from"),
            (OsmId::Node(NodeId(5)), "via"),
            (OsmId::Way(WayId(21)), "to"),
        ]
    );
}

#[test]
fn reads_gzip_compressed_change_files() {
    let dir = TempDir::new("osm-change");
    let path = dir.join("day.osc.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(CHANGE.as_bytes()).expect("gzip writes");
    std::fs::write(&path, encoder.finish().expect("gzip finishes")).expect("file writes");
    let plain = dir.join("day.osc");
    std::fs::write(&plain, CHANGE).expect("file writes");

    assert_eq!(
        read_osm_change_file(&path).expect("gzip change reads"),
        read_osm_change_file(&plain).expect("plain change reads")
    );
}

#[test]
fn keeps_the_newest_version_of_each_object() {
    let changes = parse_osm_change(
        r#"<osmChange version="0.6">
          <create><node id="7" version="1" lat="1" lon="1"/></create>
          <modify><node id="7" version="3" lat="3" lon="3"/></modify>
          <modify><node id="7" version="2" lat="2" lon="2"/></modify>
          <modify><way id="7" version="5"><nd ref="7"/><nd ref="8"/></way></modify>
          <delete><way id="7" version="6"/></delete>
        </osmChange>"#
            .as_bytes(),
    )
    .expect("change parses");
    let effective = effective_changes(changes);
    assert_eq!(effective.len(), 2);
    let OsmObj::Node(node) = &effective[0].object else {
        panic!("nodes sort first");
    };
    assert_eq!((effective[0].version, node.decimicro_lat), (3, 30_000_000));
    assert_eq!(
        (effective[1].action, effective[1].version),
        (ChangeAction::Delete, 6)
    );
}

#[test]
fn rejects_documents_that_are_not_change_files() {
    for document in [
        r#"<osm version="0.6"><node id="1" lat="0" lon="0"/></osm>"#,
        r#"<osmChange><node id="1" version="1" lat="0" lon="0"/></osmChange>"#,
        r#"<osmChange><modify><node id="1" version="1"/></modify></osmChange>"#,
        r#"<osmChange><modify><way id="1"><nd ref="x"/></way></modify></osmChange>"#,
        r#"<osmChange><modify><way id="1"><nd ref="2"/></modify></osmChange>"#,
    ] {
        assert!(parse_osm_change(document.as_bytes()).is_err(), "{document}");
    }
}

#[test]
fn parses_decimal_degrees_exactly() {
    for (text, expected) in [
        ("53.2194120", 532_194_120),
        ("53.219412", 532_194_120),
        ("-0.0000001", -1),
        ("6", 60_000_000),
        (".5", 5_000_000),
        ("+1.00000005", 10_000_001),
        ("-1.00000004", -10_000_000),
    ] {
        assert_eq!(parse_decimicro(text).expect(text), expected, "{text}");
    }
    for text in ["", "-", "1.2.3", "1e5", "300", "abc"] {
        assert!(parse_decimicro(text).is_err(), "{text}");
    }
}
