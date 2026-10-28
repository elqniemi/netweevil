//! Reader for OSM change files (`.osc` / `.osc.gz`, the osmChange XML that
//! OSM replication and Geofabrik update feeds publish).
//!
//! Objects are returned as `osmpbfreader` values, so classification and
//! restriction parsing are the same code the PBF importer runs.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use osmpbfreader::{Node, NodeId, OsmId, OsmObj, Ref, Relation, RelationId, Tags, Way, WayId};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeAction {
    Create,
    Modify,
    Delete,
}

/// One object version from a change file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OsmChange {
    pub(crate) action: ChangeAction,
    /// Object version; `0` when the file omits it.
    pub(crate) version: u64,
    pub(crate) object: OsmObj,
}

/// Reads a change file, inflating it when it starts with the gzip magic.
pub(crate) fn read_osm_change_file(path: &Path) -> Result<Vec<OsmChange>> {
    let mut file = BufReader::new(
        File::open(path).with_context(|| format!("opening change file {}", path.display()))?,
    );
    let gzip = file
        .fill_buf()
        .with_context(|| format!("reading change file {}", path.display()))?
        .starts_with(&[0x1f, 0x8b]);
    let reader: Box<dyn Read> = if gzip {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    parse_osm_change(BufReader::new(reader))
        .with_context(|| format!("parsing change file {}", path.display()))
}

/// Parses osmChange XML into object versions in document order.
pub(crate) fn parse_osm_change(input: impl BufRead) -> Result<Vec<OsmChange>> {
    let mut reader = Reader::from_reader(input);
    let mut buffer = Vec::new();
    let mut changes = Vec::new();
    let mut root_seen = false;
    let mut action = None;
    let mut open: Option<OsmChange> = None;

    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .with_context(|| format!("at byte {}", reader.buffer_position()))?;
        match event {
            Event::Eof => break,
            Event::Start(ref element) | Event::Empty(ref element) => {
                let self_closing = matches!(event, Event::Empty(_));
                let name = element.local_name();
                match name.as_ref() {
                    "osmChange" => root_seen = true,
                    "create" | "modify" | "delete" if root_seen => {
                        action = Some(match name.as_ref() {
                            "create" => ChangeAction::Create,
                            "modify" => ChangeAction::Modify,
                            _ => ChangeAction::Delete,
                        });
                    }
                    "node" | "way" | "relation" => {
                        if open.is_some() {
                            bail!("nested OSM object in change file");
                        }
                        let Some(action) = action else {
                            bail!("OSM object outside a create, modify or delete block");
                        };
                        let change = start_object(element, action)?;
                        if self_closing {
                            changes.push(change);
                        } else {
                            open = Some(change);
                        }
                    }
                    "tag" => {
                        if let Some(change) = open.as_mut() {
                            let key = attribute(element, "k")?.context("tag without k")?;
                            let value = attribute(element, "v")?.context("tag without v")?;
                            tags_mut(&mut change.object).insert(key.into(), value.into());
                        }
                    }
                    "nd" => match open.as_mut().map(|change| &mut change.object) {
                        Some(OsmObj::Way(way)) => {
                            way.nodes.push(NodeId(required_i64(element, "ref")?));
                        }
                        _ => bail!("<nd> outside a way"),
                    },
                    "member" => match open.as_mut().map(|change| &mut change.object) {
                        Some(OsmObj::Relation(relation)) => {
                            let id = required_i64(element, "ref")?;
                            let member = match attribute(element, "type")?.as_deref() {
                                Some("node") => OsmId::Node(NodeId(id)),
                                Some("way") => OsmId::Way(WayId(id)),
                                Some("relation") => OsmId::Relation(RelationId(id)),
                                other => bail!("unknown relation member type {other:?}"),
                            };
                            let role = attribute(element, "role")?.unwrap_or_default();
                            relation.refs.push(Ref {
                                member,
                                role: role.into(),
                            });
                        }
                        _ => bail!("<member> outside a relation"),
                    },
                    _ => {}
                }
            }
            Event::End(element) => match element.local_name().as_ref() {
                "node" | "way" | "relation" => {
                    changes.push(open.take().context("unbalanced OSM object end tag")?);
                }
                "create" | "modify" | "delete" => action = None,
                _ => {}
            },
            _ => {}
        }
        buffer.clear();
    }
    if !root_seen {
        bail!("not an osmChange document");
    }
    if open.is_some() {
        bail!("change file ends inside an OSM object");
    }
    Ok(changes)
}

/// Keeps the newest version of every object (the later one on equal
/// versions), as `osmium apply-changes` does, ordered by object kind and id.
pub(crate) fn effective_changes(changes: Vec<OsmChange>) -> Vec<OsmChange> {
    let mut latest = BTreeMap::<(u8, i64), OsmChange>::new();
    for change in changes {
        let key = object_key(&change.object);
        if latest
            .get(&key)
            .is_none_or(|existing| change.version >= existing.version)
        {
            latest.insert(key, change);
        }
    }
    latest.into_values().collect()
}

fn object_key(object: &OsmObj) -> (u8, i64) {
    match object {
        OsmObj::Node(node) => (0, node.id.0),
        OsmObj::Way(way) => (1, way.id.0),
        OsmObj::Relation(relation) => (2, relation.id.0),
    }
}

fn start_object(element: &BytesStart<'_>, action: ChangeAction) -> Result<OsmChange> {
    let id = required_i64(element, "id")?;
    let version = match attribute(element, "version")? {
        Some(value) => value
            .parse::<u64>()
            .with_context(|| format!("invalid version '{value}'"))?,
        None => 0,
    };
    let object = match element.local_name().as_ref() {
        "node" => {
            let coordinate = |name: &str| -> Result<i32> {
                match attribute(element, name)? {
                    Some(value) => parse_decimicro(&value),
                    // Deleted nodes may carry no position.
                    None if action == ChangeAction::Delete => Ok(0),
                    None => bail!("node {id} has no coordinates"),
                }
            };
            OsmObj::Node(Node {
                id: NodeId(id),
                tags: Tags::new(),
                decimicro_lat: coordinate("lat")?,
                decimicro_lon: coordinate("lon")?,
            })
        }
        "way" => OsmObj::Way(Way {
            id: WayId(id),
            tags: Tags::new(),
            nodes: Vec::new(),
        }),
        _ => OsmObj::Relation(Relation {
            id: RelationId(id),
            tags: Tags::new(),
            refs: Vec::new(),
        }),
    };
    Ok(OsmChange {
        action,
        version,
        object,
    })
}

fn tags_mut(object: &mut OsmObj) -> &mut Tags {
    match object {
        OsmObj::Node(node) => &mut node.tags,
        OsmObj::Way(way) => &mut way.tags,
        OsmObj::Relation(relation) => &mut relation.tags,
    }
}

fn attribute(element: &BytesStart<'_>, name: &str) -> Result<Option<String>> {
    for attribute in element.attributes() {
        let attribute = attribute.context("malformed XML attribute")?;
        if attribute.key.local_name().as_ref() == name {
            let value = attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .context("malformed XML attribute value")?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn required_i64(element: &BytesStart<'_>, name: &str) -> Result<i64> {
    let value = attribute(element, name)?
        .with_context(|| format!("<{}> without {}", element.local_name().as_ref(), name))?;
    value
        .parse()
        .with_context(|| format!("invalid integer '{value}'"))
}

/// Parses a decimal degree string into 1e-7 degree units exactly, the
/// representation PBF files store, rounding half away from zero beyond
/// seven decimals.
pub(crate) fn parse_decimicro(value: &str) -> Result<i32> {
    let invalid = || anyhow::anyhow!("invalid coordinate '{value}'");
    let trimmed = value.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    let mut units: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse::<i64>().map_err(|_| invalid())?
    };
    for position in 0..7 {
        let digit = fraction
            .as_bytes()
            .get(position)
            .map_or(0, |byte| byte - b'0');
        units = units
            .checked_mul(10)
            .and_then(|units| units.checked_add(i64::from(digit)))
            .ok_or_else(invalid)?;
    }
    if fraction.as_bytes().get(7).is_some_and(|byte| *byte >= b'5') {
        units += 1;
    }
    let units = if negative { -units } else { units };
    i32::try_from(units).map_err(|_| invalid())
}

#[cfg(test)]
mod tests;
