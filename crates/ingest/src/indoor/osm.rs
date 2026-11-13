//! OpenStreetMap Simple Indoor Tagging to an engine input.
//!
//! | OSM | Engine |
//! | --- | ------ |
//! | `indoor=room` (closed way or multipolygon) | surface with walls, class `room` |
//! | `indoor=corridor` | surface with walls, class `corridor` |
//! | `indoor=area` | open surface (no walls), class `area` |
//! | `door=*`, `indoor=door`, `entrance=*` node on a surface outline | portal across that outline |
//! | `highway=corridor\|footway\|path\|steps` way with `level` or `indoor=yes` | edge; a way on several levels is a vertical (stairs, ramp or escalator) |
//! | `highway=elevator` node, `highway=elevator` or `stairwell=*` area | vertical (lift or stairs) with a stop on each level |
//! | `conveying=*` | escalator (vertical) or travelator (edge with a fixed speed) |
//! | `indoor=wall`, `barrier=*` way with `level` | barrier |
//! | named rooms, `amenity`/`shop`/`office` features | POIs with `name` and `ref` |
//!
//! Every feature is repeated on each level of `level` and `repeat_on`
//! (`0;1`, `1-3`, `-1`, `0.5`). Level `n` has `z = n * storey height`, where
//! the storey height is `height / building:levels` of the building around the
//! site when both tags are present, else [`STOREY_HEIGHT_M`]. Every element
//! keeps its OSM id and tags in `props`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_core::LocalFrame;
use netweevil_indoor::Input;
use netweevil_indoor::input::{Access, Barrier, Edge, Level, Poi, Portal, Stop, Surface, Vertical};
use osmpbfreader::{NodeId, OsmId, OsmObj, Ref, Relation, RelationId, Tags, Way, WayId};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde::Serialize;
use serde_json::{Value, json};

/// Storey height used when the building has no `height` and `building:levels`.
pub const STOREY_HEIGHT_M: f64 = 3.5;
/// Width of a door without a `width` tag, metres.
const DOOR_WIDTH_M: f64 = 0.9;
/// Width of an entrance without a `width` tag, metres.
const ENTRANCE_WIDTH_M: f64 = 1.2;
/// Speed on a travelator (walking plus the belt), m/s.
const TRAVELATOR_SPEED: f64 = 2.0;
/// Margin around the indoor surfaces within which edges, lifts, barriers and
/// POIs are kept, metres.
const SITE_MARGIN_M: f64 = 25.0;

/// What an OSM import produced.
#[derive(Debug, Clone, Default, Serialize)]
pub struct OsmIndoorStats {
    pub rooms: usize,
    pub corridors: usize,
    pub areas: usize,
    pub doors: usize,
    pub entrances: usize,
    pub edges: usize,
    pub stairs: usize,
    pub lifts: usize,
    pub escalators: usize,
    pub travelators: usize,
    pub barriers: usize,
    pub pois: usize,
    pub levels: usize,
    pub storey_height_m: f64,
    /// Doors and entrances that are not on any surface outline of their level.
    pub skipped_doors: usize,
}

const POI_KEYS: [&str; 3] = ["amenity", "shop", "office"];

fn tag<'a>(tags: &'a Tags, key: &str) -> Option<&'a str> {
    tags.get(key).map(|v| v.as_str())
}

fn has_level(tags: &Tags) -> bool {
    tags.contains_key("level") || tags.contains_key("repeat_on")
}

fn surface_class(tags: &Tags) -> Option<&'static str> {
    match tag(tags, "indoor") {
        Some("room") => Some(if tags.contains_key("stairwell") {
            "stairs"
        } else if tag(tags, "highway") == Some("elevator") {
            "lift"
        } else {
            "room"
        }),
        Some("corridor") => Some("corridor"),
        Some("area") => Some("area"),
        _ if tags.contains_key("stairwell") && has_level(tags) => {
            Some(if tag(tags, "stairwell") == Some("elevator") {
                "lift"
            } else {
                "stairs"
            })
        }
        _ if tag(tags, "highway") == Some("elevator") && has_level(tags) => Some("lift"),
        _ => None,
    }
}

fn is_door(tags: &Tags) -> bool {
    tags.contains_key("door") || tag(tags, "indoor") == Some("door") || is_entrance(tags)
}

fn is_entrance(tags: &Tags) -> bool {
    tags.get("entrance").is_some_and(|v| v != "no")
}

fn is_walk_way(tags: &Tags) -> bool {
    matches!(
        tag(tags, "highway"),
        Some("corridor" | "footway" | "path" | "steps" | "pedestrian")
    ) && (has_level(tags) || tag(tags, "indoor") == Some("yes"))
}

fn is_barrier_way(tags: &Tags) -> bool {
    tag(tags, "indoor") == Some("wall") || (tags.contains_key("barrier") && has_level(tags))
}

fn is_poi(tags: &Tags) -> bool {
    POI_KEYS.iter().any(|k| tags.contains_key(*k)) && has_level(tags)
}

/// Whether an object can contribute to an indoor site (a building with a
/// height is kept for the storey height).
pub(crate) fn is_indoor_relevant(obj: &OsmObj) -> bool {
    let tags = obj.tags();
    match obj {
        OsmObj::Node(_) => {
            is_door(tags) || tag(tags, "highway") == Some("elevator") || is_poi(tags)
        }
        OsmObj::Way(_) => {
            surface_class(tags).is_some()
                || is_walk_way(tags)
                || is_barrier_way(tags)
                || is_poi(tags)
                || (tags.contains_key("building")
                    && tags.contains_key("height")
                    && tags.contains_key("building:levels"))
        }
        OsmObj::Relation(_) => {
            tag(tags, "type") == Some("multipolygon") && surface_class(tags).is_some()
        }
    }
}

/// Parses `level` and `repeat_on`: `0;1`, `1-3`, `-2--1`, `0.5`. Ranges of
/// whole numbers include every level between their ends.
pub(crate) fn parse_levels(tags: &Tags) -> Vec<f64> {
    let mut out: Vec<f64> = vec![];
    for key in ["level", "repeat_on"] {
        let Some(v) = tag(tags, key) else { continue };
        for part in v.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            if let Ok(x) = part.parse::<f64>() {
                out.push(x);
                continue;
            }
            // A range: the separating '-' is not the first character.
            let Some(k) = part[1..].find('-').map(|k| k + 1) else {
                continue;
            };
            let (a, b) = (
                part[..k].trim().parse::<f64>(),
                part[k + 1..].trim().parse::<f64>(),
            );
            let (Ok(a), Ok(b)) = (a, b) else { continue };
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if lo.fract() == 0.0 && hi.fract() == 0.0 && hi - lo <= 200.0 {
                let mut l = lo;
                while l <= hi {
                    out.push(l);
                    l += 1.0;
                }
            } else {
                out.push(lo);
                out.push(hi);
            }
        }
    }
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// Level id of a level number: `0`, `-1`, `0.5`.
pub(crate) fn level_id(l: f64) -> String {
    if l.fract() == 0.0 {
        format!("{}", l as i64)
    } else {
        format!("{l}")
    }
}

fn parse_width(tags: &Tags) -> Option<f64> {
    let v = tag(tags, "width")?.trim().to_ascii_lowercase();
    let (num, scale) = if let Some(n) = v.strip_suffix("cm") {
        (n.trim(), 0.01)
    } else if let Some(n) = v.strip_suffix('m') {
        (n.trim(), 1.0)
    } else {
        (v.as_str(), 1.0)
    };
    num.replace(',', ".")
        .parse::<f64>()
        .ok()
        .map(|w| w * scale)
        .filter(|w| *w > 0.0)
}

fn parse_metres(tags: &Tags, key: &str) -> Option<f64> {
    let v = tag(tags, key)?.trim().trim_end_matches('m').trim();
    v.replace(',', ".").parse::<f64>().ok().filter(|x| *x > 0.0)
}

fn tags_json(tags: &Tags) -> Value {
    let map: BTreeMap<&str, &str> = tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    json!(map)
}

fn props(osm_id: &str, tags: &Tags, level: Option<&str>) -> Value {
    let mut p = json!({ "osm_id": osm_id, "tags": tags_json(tags) });
    for key in ["name", "ref"] {
        if let Some(v) = tag(tags, key) {
            p[key] = json!(v);
        }
    }
    if let Some(l) = level {
        p["level"] = json!(l);
    }
    p
}

/// Id of a feature repeated on several levels: `w12` on one level, `w12@1` on many.
fn feature_id(base: &str, level: f64, levels: usize) -> String {
    if levels > 1 {
        format!("{base}@{}", level_id(level))
    } else {
        base.to_string()
    }
}

struct Ctx<'a, 'b> {
    nodes: &'b HashMap<i64, (f64, f64, &'a Tags)>,
    frame: LocalFrame,
}

impl Ctx<'_, '_> {
    fn xy(&self, id: i64) -> Option<[f64; 2]> {
        let (lon, lat, _) = self.nodes.get(&id)?;
        Some(self.frame.to_local(*lon, *lat))
    }
    fn ring(&self, ids: &[i64]) -> Option<Vec<Vec<f64>>> {
        ids.iter()
            .map(|id| self.xy(*id).map(|p| vec![p[0], p[1]]))
            .collect()
    }
}

fn closed(nodes: &[NodeId]) -> bool {
    nodes.len() >= 4 && nodes.first() == nodes.last()
}

/// Joins way node lists into closed rings by shared end nodes.
fn assemble_rings(mut parts: Vec<Vec<i64>>) -> Vec<Vec<i64>> {
    let mut rings = vec![];
    while let Some(mut ring) = parts.pop() {
        loop {
            if ring.len() >= 4 && ring.first() == ring.last() {
                rings.push(ring);
                break;
            }
            let end = *ring.last().expect("non-empty ring");
            let next = parts
                .iter()
                .position(|p| p.first() == Some(&end) || p.last() == Some(&end));
            let Some(k) = next else { break };
            let mut p = parts.swap_remove(k);
            if p.first() != Some(&end) {
                p.reverse();
            }
            ring.extend(p.into_iter().skip(1));
        }
    }
    rings
}

/// One surface feature (a way or a multipolygon), in node ids.
struct SurfaceFeature<'a> {
    base: String,
    tags: &'a Tags,
    class: &'static str,
    /// Polygons: outer ring then holes.
    polygons: Vec<Vec<Vec<i64>>>,
    levels: Vec<f64>,
}

/// Reads an `.osm.pbf` or `.osm` file and builds one site from its indoor
/// features (within `bbox` when given).
pub fn read_osm_indoor(
    path: &Path,
    anchor: Option<LocalFrame>,
    bbox: Option<[f64; 4]>,
) -> Result<(Input, LocalFrame, OsmIndoorStats)> {
    let name = path.to_string_lossy().to_ascii_lowercase();
    let objects: Vec<OsmObj> = if name.ends_with(".pbf") {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut reader = osmpbfreader::OsmPbfReader::new(BufReader::new(file));
        reader
            .get_objs_and_deps(is_indoor_relevant)
            .with_context(|| format!("reading {}", path.display()))?
            .into_values()
            .collect()
    } else {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        parse_osm_xml(BufReader::new(file))
            .with_context(|| format!("reading {}", path.display()))?
    };
    osm_indoor_input(&objects, anchor, bbox)
}

/// Builds one site input in local metres from OSM objects. The frame is
/// `anchor`, else the centre of the indoor surfaces (or of all features).
pub fn osm_indoor_input(
    objects: &[OsmObj],
    anchor: Option<LocalFrame>,
    bbox: Option<[f64; 4]>,
) -> Result<(Input, LocalFrame, OsmIndoorStats)> {
    let mut nodes = HashMap::new();
    let mut ways: Vec<&Way> = vec![];
    let mut way_by_id: HashMap<i64, &Way> = HashMap::new();
    let mut relations: Vec<&Relation> = vec![];
    for obj in objects {
        match obj {
            OsmObj::Node(n) => {
                nodes.insert(n.id.0, (n.lon(), n.lat(), &n.tags));
            }
            OsmObj::Way(w) => {
                ways.push(w);
                way_by_id.insert(w.id.0, w);
            }
            OsmObj::Relation(r) => relations.push(r),
        }
    }
    let in_bbox = |id: i64| match (bbox, nodes.get(&id)) {
        (None, Some(_)) => true,
        (Some(b), Some((lon, lat, _))) => {
            *lon >= b[0] && *lon <= b[2] && *lat >= b[1] && *lat <= b[3]
        }
        _ => false,
    };

    // Surfaces.
    let mut surfaces: Vec<SurfaceFeature> = vec![];
    for w in &ways {
        let Some(class) = surface_class(&w.tags) else {
            continue;
        };
        if !closed(&w.nodes) || !w.nodes.iter().all(|n| nodes.contains_key(&n.0)) {
            continue;
        }
        if !in_bbox(w.nodes[0].0) {
            continue;
        }
        surfaces.push(SurfaceFeature {
            base: format!("w{}", w.id.0),
            tags: &w.tags,
            class,
            polygons: vec![vec![w.nodes.iter().map(|n| n.0).collect()]],
            levels: parse_levels(&w.tags),
        });
    }
    for r in &relations {
        let Some(class) = surface_class(&r.tags) else {
            continue;
        };
        let members = |role_outer: bool| -> Vec<Vec<i64>> {
            r.refs
                .iter()
                .filter(|m| (m.role.as_str() != "inner") == role_outer)
                .filter_map(|m| match m.member {
                    OsmId::Way(id) => way_by_id.get(&id.0),
                    _ => None,
                })
                .map(|w| w.nodes.iter().map(|n| n.0).collect())
                .collect()
        };
        let outers = assemble_rings(members(true));
        let inners = assemble_rings(members(false));
        if outers.is_empty()
            || !outers
                .iter()
                .chain(&inners)
                .flatten()
                .all(|n| nodes.contains_key(n))
            || !in_bbox(outers[0][0])
        {
            continue;
        }
        let polygons = outers
            .into_iter()
            .map(|o| {
                let mut poly = vec![o];
                poly.extend(inners.iter().cloned());
                poly
            })
            .collect();
        surfaces.push(SurfaceFeature {
            base: format!("r{}", r.id.0),
            tags: &r.tags,
            class,
            polygons,
            levels: parse_levels(&r.tags),
        });
    }
    surfaces.retain(|s| !s.levels.is_empty());

    // The extent of the surfaces; other features are kept near it.
    let mut ext = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for id in surfaces
        .iter()
        .flat_map(|s| s.polygons.iter().flatten().flatten())
    {
        let (lon, lat, _) = nodes[id];
        ext = [
            ext[0].min(lon),
            ext[1].min(lat),
            ext[2].max(lon),
            ext[3].max(lat),
        ];
    }
    let has_surfaces = ext[0].is_finite();
    let margin_lat = SITE_MARGIN_M / 111_320.0;
    let near_site = |id: i64| -> bool {
        if !in_bbox(id) {
            return false;
        }
        if !has_surfaces {
            return true;
        }
        let (lon, lat, _) = nodes[&id];
        let margin_lon = margin_lat / lat.to_radians().cos().max(0.01);
        lon >= ext[0] - margin_lon
            && lon <= ext[2] + margin_lon
            && lat >= ext[1] - margin_lat
            && lat <= ext[3] + margin_lat
    };
    let walk_ways: Vec<&&Way> = ways
        .iter()
        .filter(|w| is_walk_way(&w.tags) && w.nodes.len() >= 2)
        .filter(|w| w.nodes.iter().all(|n| nodes.contains_key(&n.0)))
        .filter(|w| w.nodes.iter().any(|n| near_site(n.0)))
        .collect();
    if surfaces.is_empty() && walk_ways.is_empty() {
        bail!(
            "no indoor features found{}: expected indoor=room|corridor|area areas or highway=corridor|footway|steps ways with level",
            if bbox.is_some() { " in the bbox" } else { "" }
        );
    }

    let frame = match anchor {
        Some(a) => a,
        None => {
            let ids: Vec<i64> = if has_surfaces {
                surfaces
                    .iter()
                    .flat_map(|s| s.polygons.iter().flatten().flatten().copied())
                    .collect()
            } else {
                walk_ways
                    .iter()
                    .flat_map(|w| w.nodes.iter().map(|n| n.0))
                    .collect()
            };
            let (mut a, mut b) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for id in ids {
                let (lon, lat, _) = nodes[&id];
                a = [a[0].min(lon), a[1].min(lat)];
                b = [b[0].max(lon), b[1].max(lat)];
            }
            LocalFrame::new((a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5)
        }
    };

    // Storey height from the building around the site centre.
    let centre = (frame.lon, frame.lat);
    let storey = ways
        .iter()
        .filter(|w| w.tags.contains_key("building"))
        .filter_map(|w| {
            let h = parse_metres(&w.tags, "height")?;
            let n = tag(&w.tags, "building:levels")?
                .trim()
                .parse::<f64>()
                .ok()?;
            let pts: Vec<(f64, f64)> = w
                .nodes
                .iter()
                .filter_map(|n| nodes.get(&n.0).map(|(x, y, _)| (*x, *y)))
                .collect();
            let inside = pts.len() >= 3 && {
                let (lo_x, hi_x) = pts
                    .iter()
                    .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                let (lo_y, hi_y) = pts
                    .iter()
                    .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                centre.0 >= lo_x && centre.0 <= hi_x && centre.1 >= lo_y && centre.1 <= hi_y
            };
            (inside && n >= 1.0).then_some(h / n)
        })
        .next()
        .unwrap_or(STOREY_HEIGHT_M);

    let ctx = Ctx {
        nodes: &nodes,
        frame,
    };
    let mut input = Input {
        version: 1,
        crs: Some("local".into()),
        ..Input::default()
    };
    let mut stats = OsmIndoorStats {
        storey_height_m: storey,
        ..OsmIndoorStats::default()
    };
    let mut levels: BTreeSet<i64> = BTreeSet::new();
    // Level numbers as integer keys of 1/1000 to keep them ordered and exact.
    let key = |l: f64| (l * 1000.0).round() as i64;
    let use_level = |levels: &mut BTreeSet<i64>, l: f64| -> String {
        levels.insert(key(l));
        level_id(l)
    };

    // (node id, level key) -> surface ids whose outline passes the node, with
    // the outline neighbours of the node for the door direction.
    let mut outline: HashMap<(i64, i64), Vec<(String, [i64; 2])>> = HashMap::new();
    for s in &surfaces {
        match s.class {
            "room" | "stairs" | "lift" => stats.rooms += 1,
            "corridor" => stats.corridors += 1,
            _ => stats.areas += 1,
        }
        for &l in &s.levels {
            let id = feature_id(&s.base, l, s.levels.len());
            let level = use_level(&mut levels, l);
            let mut polys = vec![];
            for poly in &s.polygons {
                let rings: Option<Vec<_>> = poly.iter().map(|r| ctx.ring(r)).collect();
                if let Some(rings) = rings {
                    polys.push(rings);
                }
                for ring in poly {
                    let n = ring.len() - 1;
                    for i in 0..n {
                        let prev = ring[(i + n - 1) % n];
                        let next = ring[(i + 1) % n];
                        outline
                            .entry((ring[i], key(l)))
                            .or_default()
                            .push((id.clone(), [prev, next]));
                    }
                }
            }
            let (polygon, multipolygon) = if polys.len() == 1 {
                (polys.pop(), None)
            } else {
                (None, Some(polys))
            };
            input.surfaces.push(Surface {
                id: Some(id.clone()),
                level: Some(level.clone()),
                polygon,
                multipolygon,
                space: Some(id.clone()),
                class: Some(s.class.into()),
                walls: s.class != "area",
                tags: vec!["indoor".into()],
                props: props(&s.base, s.tags, Some(&level)),
                ..Surface::default()
            });
            // Lifts and stairwells drawn as areas carry a vertical at their centre.
            let named = tag(s.tags, "name").is_some() || tag(s.tags, "ref").is_some();
            let poi_key = POI_KEYS.iter().find(|k| s.tags.contains_key(**k));
            if (named || poi_key.is_some()) && !matches!(s.class, "lift" | "stairs") {
                let category = match poi_key {
                    Some(k) => format!("{k}/{}", s.tags[*k]),
                    None => s.class.to_string(),
                };
                input.pois.push(Poi {
                    id: Some(id.clone()),
                    category: Some(category),
                    access: vec![Access {
                        space: Some(id.clone()),
                        ..Access::default()
                    }],
                    props: props(&s.base, s.tags, Some(&level)),
                    ..Poi::default()
                });
                stats.pois += 1;
            }
        }
        if matches!(s.class, "lift" | "stairs") && s.levels.len() >= 2 {
            let ring = &s.polygons[0][0];
            let pts: Vec<[f64; 2]> = ring[..ring.len() - 1]
                .iter()
                .filter_map(|n| ctx.xy(*n))
                .collect();
            let c = [
                pts.iter().map(|p| p[0]).sum::<f64>() / pts.len() as f64,
                pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64,
            ];
            let lift = s.class == "lift";
            input.verticals.push(Vertical {
                id: Some(format!("{}:vertical", s.base)),
                kind: Some(if lift { "lift" } else { "stairs" }.into()),
                stops: s
                    .levels
                    .iter()
                    .map(|l| Stop {
                        level: Some(level_id(*l)),
                        at: vec![c[0], c[1]],
                    })
                    .collect(),
                step_free: Some(lift),
                direction: Some("both".into()),
                tags: vec![if lift { "lift" } else { "stairs" }.into()],
                props: props(&s.base, s.tags, None),
                ..Vertical::default()
            });
            if lift {
                stats.lifts += 1;
            } else {
                stats.stairs += 1;
            }
        }
    }

    // Doors and entrances on surface outlines.
    let mut door_nodes: Vec<(&i64, &(f64, f64, &Tags))> =
        ctx.nodes.iter().filter(|(_, n)| is_door(n.2)).collect();
    door_nodes.sort_by_key(|(id, _)| **id);
    for (&id, &(_, _, tags)) in door_nodes {
        let own = parse_levels(tags);
        let candidate: Vec<f64> = if own.is_empty() {
            levels.iter().map(|k| *k as f64 / 1000.0).collect()
        } else {
            own
        };
        let on: Vec<(f64, &Vec<(String, [i64; 2])>)> = candidate
            .iter()
            .filter_map(|l| outline.get(&(id, key(*l))).map(|v| (*l, v)))
            .collect();
        if on.is_empty() {
            stats.skipped_doors += 1;
            continue;
        }
        let entrance = is_entrance(tags);
        if entrance {
            stats.entrances += 1;
        } else {
            stats.doors += 1;
        }
        let width = parse_width(tags).unwrap_or(if entrance {
            ENTRANCE_WIDTH_M
        } else {
            DOOR_WIDTH_M
        });
        let step_free =
            tag(tags, "wheelchair") != Some("no") && tag(tags, "door") != Some("revolving");
        let at = ctx.xy(id).expect("door node position");
        for (l, owners) in &on {
            let [prev, next] = owners[0].1;
            let (Some(a), Some(b)) = (ctx.xy(prev), ctx.xy(next)) else {
                continue;
            };
            let d = [b[0] - a[0], b[1] - a[1]];
            let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-9);
            let h = width * 0.5 / len;
            let mut spaces: Vec<String> = owners.iter().map(|o| o.0.clone()).collect();
            spaces.dedup();
            let mut ptags = vec![];
            if entrance {
                ptags.push("entrance".to_string());
            }
            if let Some(d) = tag(tags, "door") {
                ptags.push(format!("door:{d}"));
            }
            let level = level_id(*l);
            input.portals.push(Portal {
                id: Some(feature_id(&format!("n{id}"), *l, on.len())),
                kind: Some(if entrance { "entrance" } else { "door" }.into()),
                segment: Some(vec![
                    vec![at[0] - d[0] * h, at[1] - d[1] * h],
                    vec![at[0] + d[0] * h, at[1] + d[1] * h],
                ]),
                level: Some(level.clone()),
                spaces,
                width: Some(width),
                step_free,
                tags: ptags,
                props: props(&format!("n{id}"), tags, Some(&level)),
                ..Portal::default()
            });
        }
    }

    // Walkable ways: edges on one level, verticals across levels.
    let z_of = |l: f64| l * storey;
    for w in walk_ways {
        let lv = parse_levels(&w.tags);
        let lv = if lv.is_empty() { vec![0.0] } else { lv };
        let ids: Vec<i64> = w.nodes.iter().map(|n| n.0).collect();
        let pts: Vec<[f64; 2]> = ids.iter().filter_map(|n| ctx.xy(*n)).collect();
        let highway = tag(&w.tags, "highway").unwrap_or("footway");
        let conveying = tag(&w.tags, "conveying").filter(|v| *v != "no");
        let base = format!("w{}", w.id.0);
        if lv.len() >= 2 {
            // The way runs from its first to its last level; `incline=down`
            // runs from the highest level down.
            let (lo, hi) = (lv[0], lv[lv.len() - 1]);
            let down = matches!(tag(&w.tags, "incline"), Some("down"));
            let (l0, l1) = if down { (hi, lo) } else { (lo, hi) };
            let total: f64 = pts
                .windows(2)
                .map(|p| dist(p[0], p[1]))
                .sum::<f64>()
                .max(1e-9);
            let mut run = 0.0;
            let mut line = vec![];
            for (i, p) in pts.iter().enumerate() {
                if i > 0 {
                    run += dist(pts[i - 1], *p);
                }
                let t = run / total;
                line.push(vec![p[0], p[1], z_of(l0) + (z_of(l1) - z_of(l0)) * t]);
            }
            let (kind, step_free) = match (conveying, highway) {
                (Some(_), _) => ("escalator", false),
                (None, "steps") => ("stairs", false),
                _ => ("ramp", true),
            };
            // Escalator direction: `forward` moves along the way.
            let direction = match conveying {
                Some("forward") => {
                    if l1 > l0 {
                        "up"
                    } else {
                        "down"
                    }
                }
                Some("backward") => {
                    if l1 > l0 {
                        "down"
                    } else {
                        "up"
                    }
                }
                _ => "both",
            };
            match kind {
                "escalator" => stats.escalators += 1,
                "stairs" => stats.stairs += 1,
                _ => stats.edges += 1,
            }
            input.verticals.push(Vertical {
                id: Some(base.clone()),
                kind: Some(kind.into()),
                stops: vec![
                    Stop {
                        level: Some(use_level(&mut levels, l0)),
                        at: vec![pts[0][0], pts[0][1]],
                    },
                    Stop {
                        level: Some(use_level(&mut levels, l1)),
                        at: vec![pts[pts.len() - 1][0], pts[pts.len() - 1][1]],
                    },
                ],
                direction: Some(direction.into()),
                step_free: Some(step_free),
                width: parse_width(&w.tags),
                line: Some(line),
                tags: vec![kind.into()],
                props: props(&base, &w.tags, None),
                ..Vertical::default()
            });
            continue;
        }
        let l = lv[0];
        let level = use_level(&mut levels, l);
        let travelator = conveying.is_some();
        let mut line: Vec<Vec<f64>> = pts.iter().map(|p| vec![p[0], p[1]]).collect();
        if conveying == Some("backward") {
            line.reverse();
        }
        if travelator {
            stats.travelators += 1;
        } else {
            stats.edges += 1;
        }
        input.edges.push(Edge {
            id: Some(base.clone()),
            line,
            level: Some(level.clone()),
            oneway: matches!(conveying, Some("forward" | "backward"))
                || (!travelator && matches!(tag(&w.tags, "oneway"), Some("yes" | "1"))),
            class: Some(if travelator { "travelator" } else { highway }.into()),
            speed: travelator.then_some(TRAVELATOR_SPEED),
            width: parse_width(&w.tags),
            step_free: highway != "steps",
            tags: vec![if travelator { "travelator" } else { highway }.into()],
            props: props(&base, &w.tags, Some(&level)),
            ..Edge::default()
        });
    }

    // Lift nodes.
    let mut lift_nodes: Vec<(&i64, &(f64, f64, &Tags))> = ctx
        .nodes
        .iter()
        .filter(|(id, n)| tag(n.2, "highway") == Some("elevator") && near_site(**id))
        .collect();
    lift_nodes.sort_by_key(|(id, _)| **id);
    for (&id, &(_, _, tags)) in lift_nodes {
        let lv = parse_levels(tags);
        if lv.len() < 2 {
            continue;
        }
        let at = ctx.xy(id).expect("lift node position");
        stats.lifts += 1;
        input.verticals.push(Vertical {
            id: Some(format!("n{id}")),
            kind: Some("lift".into()),
            stops: lv
                .iter()
                .map(|l| Stop {
                    level: Some(use_level(&mut levels, *l)),
                    at: vec![at[0], at[1]],
                })
                .collect(),
            direction: Some("both".into()),
            step_free: Some(true),
            width: parse_width(tags),
            tags: vec!["lift".into()],
            props: props(&format!("n{id}"), tags, None),
            ..Vertical::default()
        });
    }

    // Walls and barriers.
    for w in &ways {
        if !is_barrier_way(&w.tags) || surface_class(&w.tags).is_some() {
            continue;
        }
        if !w.nodes.iter().all(|n| ctx.nodes.contains_key(&n.0))
            || !w.nodes.iter().any(|n| near_site(n.0))
        {
            continue;
        }
        let lv = parse_levels(&w.tags);
        let base = format!("w{}", w.id.0);
        for &l in &lv {
            let level = use_level(&mut levels, l);
            stats.barriers += 1;
            input.barriers.push(Barrier {
                id: Some(feature_id(&base, l, lv.len())),
                line: w
                    .nodes
                    .iter()
                    .filter_map(|n| ctx.xy(n.0).map(|p| vec![p[0], p[1]]))
                    .collect(),
                level: Some(level.clone()),
                tags: vec![tag(&w.tags, "barrier").unwrap_or("wall").to_string()],
                props: props(&base, &w.tags, Some(&level)),
                ..Barrier::default()
            });
        }
    }

    // Point POIs (and area POIs that are not surfaces).
    let mut poi_nodes: Vec<(&i64, &(f64, f64, &Tags))> = ctx
        .nodes
        .iter()
        .filter(|(id, n)| is_poi(n.2) && !is_door(n.2) && near_site(**id))
        .collect();
    poi_nodes.sort_by_key(|(id, _)| **id);
    let mut point_pois: Vec<(String, &Tags, [f64; 2])> = poi_nodes
        .into_iter()
        .map(|(id, n)| (format!("n{id}"), n.2, ctx.xy(*id).expect("poi position")))
        .collect();
    for w in &ways {
        if !is_poi(&w.tags) || surface_class(&w.tags).is_some() || !closed(&w.nodes) {
            continue;
        }
        let pts: Vec<[f64; 2]> = w.nodes[..w.nodes.len() - 1]
            .iter()
            .filter_map(|n| ctx.xy(n.0))
            .collect();
        if pts.len() + 1 != w.nodes.len() || !near_site(w.nodes[0].0) {
            continue;
        }
        let c = [
            pts.iter().map(|p| p[0]).sum::<f64>() / pts.len() as f64,
            pts.iter().map(|p| p[1]).sum::<f64>() / pts.len() as f64,
        ];
        point_pois.push((format!("w{}", w.id.0), &w.tags, c));
    }
    for (base, tags, at) in point_pois {
        let lv = parse_levels(tags);
        let k = POI_KEYS
            .iter()
            .find(|k| tags.contains_key(**k))
            .expect("poi key");
        for &l in &lv {
            let level = use_level(&mut levels, l);
            stats.pois += 1;
            input.pois.push(Poi {
                id: Some(feature_id(&base, l, lv.len())),
                category: Some(format!("{k}/{}", tags[*k])),
                at: Some(vec![at[0], at[1]]),
                level: Some(level.clone()),
                props: props(&base, tags, Some(&level)),
                ..Poi::default()
            });
        }
    }

    input.levels = levels
        .iter()
        .map(|k| {
            let l = *k as f64 / 1000.0;
            Level {
                id: level_id(l),
                z: z_of(l),
                props: json!({ "level": l }),
            }
        })
        .collect();
    // Level ids on elements must match `level_id` of the parsed numbers.
    stats.levels = input.levels.len();
    Ok((input, frame, stats))
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Parses OSM XML (`<osm>` with nodes, ways and relations).
pub(crate) fn parse_osm_xml(input: impl std::io::BufRead) -> Result<Vec<OsmObj>> {
    let mut reader = Reader::from_reader(input);
    let mut buffer = Vec::new();
    let mut out = Vec::new();
    let mut open: Option<OsmObj> = None;
    let mut root = false;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .with_context(|| format!("at byte {}", reader.buffer_position()))?;
        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                match e.local_name().as_ref() {
                    "osm" => root = true,
                    "node" | "way" | "relation" => {
                        let id: i64 = attr(e, "id")?.context("object without id")?.parse()?;
                        let obj = match e.local_name().as_ref() {
                            "node" => {
                                let lat = crate::osm_change::parse_decimicro(
                                    &attr(e, "lat")?.context("node without lat")?,
                                )?;
                                let lon = crate::osm_change::parse_decimicro(
                                    &attr(e, "lon")?.context("node without lon")?,
                                )?;
                                OsmObj::Node(osmpbfreader::Node {
                                    id: NodeId(id),
                                    tags: Tags::new(),
                                    decimicro_lat: lat,
                                    decimicro_lon: lon,
                                })
                            }
                            "way" => OsmObj::Way(Way {
                                id: WayId(id),
                                tags: Tags::new(),
                                nodes: vec![],
                            }),
                            _ => OsmObj::Relation(Relation {
                                id: RelationId(id),
                                tags: Tags::new(),
                                refs: vec![],
                            }),
                        };
                        if empty {
                            out.push(obj);
                        } else {
                            open = Some(obj);
                        }
                    }
                    "tag" => {
                        if let Some(obj) = open.as_mut() {
                            let k = attr(e, "k")?.context("tag without k")?;
                            let v = attr(e, "v")?.context("tag without v")?;
                            let tags = match obj {
                                OsmObj::Node(n) => &mut n.tags,
                                OsmObj::Way(w) => &mut w.tags,
                                OsmObj::Relation(r) => &mut r.tags,
                            };
                            tags.insert(k.into(), v.into());
                        }
                    }
                    "nd" => {
                        if let Some(OsmObj::Way(w)) = open.as_mut() {
                            let r: i64 = attr(e, "ref")?.context("nd without ref")?.parse()?;
                            w.nodes.push(NodeId(r));
                        }
                    }
                    "member" => {
                        if let Some(OsmObj::Relation(rel)) = open.as_mut() {
                            let r: i64 = attr(e, "ref")?.context("member without ref")?.parse()?;
                            let member = match attr(e, "type")?.as_deref() {
                                Some("node") => OsmId::Node(NodeId(r)),
                                Some("way") => OsmId::Way(WayId(r)),
                                _ => OsmId::Relation(RelationId(r)),
                            };
                            rel.refs.push(Ref {
                                member,
                                role: attr(e, "role")?.unwrap_or_default().into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                if matches!(e.local_name().as_ref(), "node" | "way" | "relation")
                    && let Some(obj) = open.take()
                {
                    out.push(obj);
                }
            }
            _ => {}
        }
        buffer.clear();
    }
    if !root {
        bail!("not an OSM XML document");
    }
    Ok(out)
}

fn attr(e: &BytesStart<'_>, name: &str) -> Result<Option<String>> {
    for a in e.attributes() {
        let a = a.context("malformed XML attribute")?;
        if a.key.local_name().as_ref() == name {
            return Ok(Some(
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .context("malformed XML attribute value")?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}
