//! Input schema. Unknown fields are ignored; every id is optional.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `[x, y]` or `[x, y, z]`.
pub type Vtx = Vec<f64>;
pub type Ring = Vec<Vtx>;
pub type Polygon = Vec<Ring>;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Input {
    pub version: u32,
    pub crs: Option<String>,
    pub levels: Vec<Level>,
    pub defaults: Defaults,
    pub surfaces: Vec<Surface>,
    pub edges: Vec<Edge>,
    pub nodes: Vec<Node>,
    pub portals: Vec<Portal>,
    pub barriers: Vec<Barrier>,
    pub verticals: Vec<Vertical>,
    pub pois: Vec<Poi>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Level {
    pub id: String,
    pub z: f64,
    pub props: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Defaults {
    pub snap: f64,
    pub edge_attach: f64,
    /// Max distance (m) from a POI access point off every surface to the edge it snaps to.
    pub poi_snap: f64,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            snap: 0.05,
            edge_attach: 1.0,
            poi_snap: 30.0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Surface {
    pub id: Option<String>,
    pub level: Option<String>,
    pub z: Option<f64>,
    pub polygon: Option<Polygon>,
    pub multipolygon: Option<Vec<Polygon>>,
    pub space: Option<String>,
    pub class: Option<String>,
    pub walls: bool,
    pub cost_factor: f64,
    pub speed: Option<f64>,
    pub tags: Vec<String>,
    /// Access groups (any-of) a query needs to use this element; empty = public.
    pub access_groups: Vec<String>,
    pub props: Value,
}

impl Default for Surface {
    fn default() -> Self {
        Surface {
            id: None,
            level: None,
            z: None,
            polygon: None,
            multipolygon: None,
            space: None,
            class: None,
            walls: true,
            cost_factor: 1.0,
            speed: None,
            tags: vec![],
            access_groups: vec![],
            props: Value::Null,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Edge {
    pub id: Option<String>,
    pub line: Vec<Vtx>,
    pub level: Option<String>,
    pub z: Option<f64>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub oneway: bool,
    pub class: Option<String>,
    pub speed: Option<f64>,
    pub time: Option<f64>,
    pub cost_factor: f64,
    pub penalty: f64,
    pub width: Option<f64>,
    pub step_free: bool,
    pub incline: Option<f64>,
    pub tags: Vec<String>,
    /// Access groups (any-of) a query needs to use this element; empty = public.
    pub access_groups: Vec<String>,
    pub props: Value,
}

impl Default for Edge {
    fn default() -> Self {
        Edge {
            id: None,
            line: vec![],
            level: None,
            z: None,
            from: None,
            to: None,
            oneway: false,
            class: None,
            speed: None,
            time: None,
            cost_factor: 1.0,
            penalty: 0.0,
            width: None,
            step_free: true,
            incline: None,
            tags: vec![],
            access_groups: vec![],
            props: Value::Null,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Node {
    pub id: Option<String>,
    pub at: Option<Vtx>,
    pub level: Option<String>,
    pub penalty: f64,
    pub tags: Vec<String>,
    /// Access groups (any-of) a query needs to use this element; empty = public.
    pub access_groups: Vec<String>,
    pub props: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Portal {
    pub id: Option<String>,
    pub kind: Option<String>,
    pub segment: Option<Vec<Vtx>>,
    pub at: Option<Vtx>,
    pub normal: Option<Vec<f64>>,
    pub level: Option<String>,
    pub z: Option<f64>,
    pub spaces: Vec<String>,
    pub penalty: f64,
    pub oneway: Option<String>,
    pub width: Option<f64>,
    pub step_free: bool,
    pub tags: Vec<String>,
    /// Access groups (any-of) a query needs to use this element; empty = public.
    pub access_groups: Vec<String>,
    pub props: Value,
}

impl Default for Portal {
    fn default() -> Self {
        Portal {
            id: None,
            kind: None,
            segment: None,
            at: None,
            normal: None,
            level: None,
            z: None,
            spaces: vec![],
            penalty: 0.0,
            oneway: None,
            width: None,
            step_free: true,
            tags: vec![],
            access_groups: vec![],
            props: Value::Null,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Barrier {
    pub id: Option<String>,
    pub line: Vec<Vtx>,
    pub level: Option<String>,
    pub z: Option<f64>,
    pub tags: Vec<String>,
    pub props: Value,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Stop {
    pub level: Option<String>,
    pub at: Vtx,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Vertical {
    pub id: Option<String>,
    pub kind: Option<String>,
    pub stops: Vec<Stop>,
    pub wait: Option<f64>,
    pub speed: Option<f64>,
    pub time: Option<f64>,
    pub direction: Option<String>,
    pub step_free: Option<bool>,
    pub width: Option<f64>,
    pub line: Option<Vec<Vtx>>,
    pub tags: Vec<String>,
    /// Access groups (any-of) a query needs to use this element; empty = public.
    pub access_groups: Vec<String>,
    pub props: Value,
}

impl Default for Vertical {
    fn default() -> Self {
        Vertical {
            id: None,
            kind: None,
            stops: vec![],
            wait: None,
            speed: None,
            time: None,
            direction: None,
            step_free: None,
            width: None,
            line: None,
            tags: vec![],
            access_groups: vec![],
            props: Value::Null,
        }
    }
}

/// A point of interest: a routing destination with one or more access points.
/// `at` (+ `level`) alone is shorthand for one access point there.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Poi {
    pub id: Option<String>,
    /// Hierarchical category path (`toilet/accessible`); `{category: "toilet"}` matches it.
    pub category: Option<String>,
    pub at: Option<Vtx>,
    pub level: Option<String>,
    pub access: Vec<Access>,
    pub tags: Vec<String>,
    /// Access groups (any-of) a query needs to use this element; empty = public.
    pub access_groups: Vec<String>,
    pub props: Value,
}

/// One way into a POI: a point (becomes a graph node anchored like an input node), an
/// existing node, a portal (both sides) or a space (its representative point).
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Access {
    pub at: Option<Vtx>,
    pub level: Option<String>,
    pub node: Option<String>,
    pub portal: Option<String>,
    pub space: Option<String>,
}

impl Input {
    pub fn from_json(s: &str) -> Result<Input, String> {
        serde_json::from_str(s).map_err(|e| format!("input: {e}"))
    }

    /// Concatenate several inputs (levels deduplicated by id; defaults from the first).
    pub fn merge(parts: Vec<Input>) -> Input {
        let mut it = parts.into_iter();
        let mut out = it.next().unwrap_or_default();
        for p in it {
            for l in p.levels {
                if !out.levels.iter().any(|o| o.id == l.id) {
                    out.levels.push(l);
                }
            }
            out.surfaces.extend(p.surfaces);
            out.edges.extend(p.edges);
            out.nodes.extend(p.nodes);
            out.portals.extend(p.portals);
            out.barriers.extend(p.barriers);
            out.verticals.extend(p.verticals);
            out.pois.extend(p.pois);
        }
        out
    }
}

/// `"{kind}#{index}"` when no id was given.
pub fn gen_id(id: &Option<String>, kind: &str, i: usize) -> String {
    match id {
        Some(s) if !s.is_empty() => s.clone(),
        _ => format!("{kind}#{i}"),
    }
}

pub fn props_str(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        v.to_string()
    }
}
