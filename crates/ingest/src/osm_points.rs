//! Tagged OSM points, such as amenities or shops, for cell aggregation and
//! as destinations of analyses.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};
use osmpbfreader::{OsmObj, OsmPbfReader};

/// One tagged node, or one tagged way at the mean of its nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmTaggedPoint {
    /// `node/<id>` or `way/<id>`.
    pub id: String,
    pub lon: f64,
    pub lat: f64,
    /// Value of the scanned tag key.
    pub value: String,
    pub name: Option<String>,
}

/// Reads the nodes and ways of an OSM PBF file that carry the tag `key`.
/// A non-empty `values` keeps only those tag values. Points are ordered by
/// id, nodes before ways.
pub fn scan_osm_tagged_points(
    path: &Path,
    key: &str,
    values: &[String],
) -> Result<Vec<OsmTaggedPoint>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = OsmPbfReader::new(BufReader::new(file));
    let wanted = |obj: &OsmObj| {
        !obj.is_relation()
            && obj.tags().get(key).is_some_and(|value| {
                values.is_empty() || values.iter().any(|v| v == value.as_str())
            })
    };
    let objects = reader
        .get_objs_and_deps(wanted)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut points = Vec::new();
    for object in objects.values().filter(|object| wanted(object)) {
        let tag = |key: &str| object.tags().get(key).map(ToString::to_string);
        let (id, lon, lat) = match object {
            OsmObj::Node(node) => (format!("node/{}", node.id.0), node.lon(), node.lat()),
            OsmObj::Way(way) => {
                let nodes = way
                    .nodes
                    .iter()
                    .filter_map(|id| objects.get(&(*id).into())?.node())
                    .collect::<Vec<_>>();
                if nodes.is_empty() {
                    continue;
                }
                let count = nodes.len() as f64;
                (
                    format!("way/{}", way.id.0),
                    nodes.iter().map(|node| node.lon()).sum::<f64>() / count,
                    nodes.iter().map(|node| node.lat()).sum::<f64>() / count,
                )
            }
            OsmObj::Relation(_) => continue,
        };
        points.push(OsmTaggedPoint {
            id,
            lon,
            lat,
            value: tag(key).unwrap_or_default(),
            name: tag("name"),
        });
    }
    Ok(points)
}
