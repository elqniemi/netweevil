//! Edit lists for update fixtures: one list renders as an osmChange
//! document and, separately, applies to an object list the way an extract
//! tool would, so update tests compare against an independently edited
//! source.

use osmpbfreader::{OsmId, OsmObj};

use crate::osm_change::ChangeAction;

/// One edit: the action and the object's new version (for deletions only
/// the kind and id matter).
pub(crate) type OsmEdit = (ChangeAction, OsmObj);

/// `objects` with `edits` applied, sorted by kind then id like a PBF
/// extract.
pub(crate) fn apply_edits(objects: &[OsmObj], edits: &[OsmEdit]) -> Vec<OsmObj> {
    let mut objects = objects.to_vec();
    for (action, object) in edits {
        objects.retain(|existing| existing.id() != object.id());
        if *action != ChangeAction::Delete {
            objects.push(object.clone());
        }
    }
    objects.sort_by_key(|object| sort_key(object.id()));
    objects
}

fn sort_key(id: OsmId) -> (u8, i64) {
    match id {
        OsmId::Node(id) => (0, id.0),
        OsmId::Way(id) => (1, id.0),
        OsmId::Relation(id) => (2, id.0),
    }
}

/// Renders `edits` as an osmChange document, one action block per edit and
/// every object at version 2.
pub(crate) fn write_osc(edits: &[OsmEdit]) -> String {
    let mut xml =
        String::from("<?xml version='1.0' encoding='UTF-8'?>\n<osmChange version=\"0.6\">\n");
    for (action, object) in edits {
        let block = match action {
            ChangeAction::Create => "create",
            ChangeAction::Modify => "modify",
            ChangeAction::Delete => "delete",
        };
        xml.push_str(&format!("  <{block}>\n"));
        let tags = |xml: &mut String, tags: &osmpbfreader::Tags| {
            for (key, value) in tags.iter() {
                xml.push_str(&format!(
                    "      <tag k=\"{}\" v=\"{}\"/>\n",
                    escape(key),
                    escape(value)
                ));
            }
        };
        match object {
            OsmObj::Node(node) => {
                xml.push_str(&format!(
                    "    <node id=\"{}\" version=\"2\" lat=\"{}\" lon=\"{}\">\n",
                    node.id.0,
                    degrees(node.decimicro_lat),
                    degrees(node.decimicro_lon)
                ));
                tags(&mut xml, &node.tags);
                xml.push_str("    </node>\n");
            }
            OsmObj::Way(way) => {
                xml.push_str(&format!("    <way id=\"{}\" version=\"2\">\n", way.id.0));
                for node in &way.nodes {
                    xml.push_str(&format!("      <nd ref=\"{}\"/>\n", node.0));
                }
                tags(&mut xml, &way.tags);
                xml.push_str("    </way>\n");
            }
            OsmObj::Relation(relation) => {
                xml.push_str(&format!(
                    "    <relation id=\"{}\" version=\"2\">\n",
                    relation.id.0
                ));
                for member in &relation.refs {
                    let kind = match member.member {
                        OsmId::Node(_) => "node",
                        OsmId::Way(_) => "way",
                        OsmId::Relation(_) => "relation",
                    };
                    xml.push_str(&format!(
                        "      <member type=\"{kind}\" ref=\"{}\" role=\"{}\"/>\n",
                        member.member.inner_id(),
                        escape(&member.role)
                    ));
                }
                tags(&mut xml, &relation.tags);
                xml.push_str("    </relation>\n");
            }
        }
        xml.push_str(&format!("  </{block}>\n"));
    }
    xml.push_str("</osmChange>\n");
    xml
}

/// Decimicro degrees as the decimal string OSM XML uses.
fn degrees(decimicro: i32) -> String {
    let sign = if decimicro < 0 { "-" } else { "" };
    let magnitude = i64::from(decimicro).abs();
    format!(
        "{sign}{}.{:07}",
        magnitude / 10_000_000,
        magnitude % 10_000_000
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}
