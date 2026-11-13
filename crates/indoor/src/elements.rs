//! Listing of compiled elements for hosts: portals, nodes, verticals and POIs with their
//! graph positions, so a host can find entrances, lift stops and POI access points.

use crate::geom::{NONE, P3};
use crate::graph::{ElemKind, FactKind, Graph};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kinds of element that `Graph::elements` lists.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ElementKind {
    Portal,
    Node,
    Vertical,
    Poi,
}

/// One listed element.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Element {
    pub id: String,
    pub kind: ElementKind,
    /// Portal kind (`door`, `entrance`, ...), vertical kind (`lift`, `stairs`, ...) or POI
    /// category; None for nodes.
    pub subkind: Option<String>,
    /// Input level of the element (POIs: the level of their first access point).
    pub level: Option<String>,
    /// Representative position: a portal's centre, a node, a vertical's first stop, a POI's
    /// first access point. None when the element has no graph position.
    pub at: Option<P3>,
    pub tags: Vec<String>,
    /// The element's input `props` (null when none).
    pub props: Value,
    /// Access groups the element needs (any-of); empty = public.
    pub access_groups: Vec<String>,
    /// Graph positions of the element: the two sides of a portal, the node itself, each stop
    /// of a vertical, each access point of a POI. Empty when compilation could not place it
    /// (see the compile warnings).
    pub points: Vec<ElementPoint>,
}

/// A graph position of a listed element.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ElementPoint {
    /// Graph node; None for a POI access point snapped onto an edge.
    pub node: Option<u32>,
    pub at: P3,
    pub level: Option<String>,
    /// Space of the surface the point lies on, else the surface id; None off surfaces.
    pub space: Option<String>,
    /// Edge the point lies on (POI access points snapped onto an edge).
    pub edge: Option<String>,
}

/// A level of the compiled graph.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Level {
    pub id: String,
    pub z: f64,
    /// The level's input `props` (null when none).
    pub props: Value,
}

fn json(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(s).unwrap_or(Value::Null)
    }
}

impl Graph {
    fn level_name(&self, i: u32) -> Option<String> {
        self.levels.get(i as usize).map(|l| l.id.clone())
    }

    /// Space of the surface under graph node `n` (else the surface id).
    fn node_space(&self, n: u32) -> Option<String> {
        let t = self.nodes[n as usize].tri;
        if t == NONE {
            return None;
        }
        let e = &self.elems[self.mesh.tri_surf[t as usize] as usize];
        Some(self.str(e.space).unwrap_or(&e.id).to_string())
    }

    fn node_point(&self, n: u32) -> ElementPoint {
        let nd = &self.nodes[n as usize];
        ElementPoint {
            node: Some(n),
            at: nd.xyz,
            level: self.level_name(nd.level),
            space: self.node_space(n),
            edge: None,
        }
    }

    /// The levels with their z, in input order.
    pub fn level_list(&self) -> Vec<Level> {
        self.levels
            .iter()
            .map(|l| Level {
                id: l.id.clone(),
                z: l.z,
                props: json(&l.props),
            })
            .collect()
    }

    /// Every element of `kind`, in input order.
    pub fn elements(&self, kind: ElementKind) -> Vec<Element> {
        if kind == ElementKind::Poi {
            return self.poi_elements();
        }
        let want = match kind {
            ElementKind::Portal => ElemKind::Portal,
            ElementKind::Node => ElemKind::Node,
            _ => ElemKind::Vertical,
        };
        // Vertical stops: the end nodes of the element's facts.
        let mut stops: Vec<Vec<u32>> = vec![];
        if want == ElemKind::Vertical {
            stops = vec![vec![]; self.elems.len()];
            for f in &self.facts {
                if f.kind == FactKind::Vertical && f.elem != NONE {
                    let s = &mut stops[f.elem as usize];
                    for n in [f.na, f.nb] {
                        if n != NONE && !s.contains(&n) {
                            s.push(n);
                        }
                    }
                }
            }
        }
        let mut out = vec![];
        for (ei, e) in self.elems.iter().enumerate() {
            if e.kind != want {
                continue;
            }
            let nodes: Vec<u32> = match want {
                ElemKind::Portal => self
                    .portals
                    .get(&e.id)
                    .map(|ns| ns.to_vec())
                    .unwrap_or_default(),
                ElemKind::Node => self.names.get(&e.id).map(|&n| vec![n]).unwrap_or_default(),
                _ => std::mem::take(&mut stops[ei]),
            };
            let points: Vec<ElementPoint> = nodes.iter().map(|&n| self.node_point(n)).collect();
            let at = match (want, points.as_slice()) {
                (ElemKind::Portal, [a, b]) => Some([
                    (a.at[0] + b.at[0]) * 0.5,
                    (a.at[1] + b.at[1]) * 0.5,
                    (a.at[2] + b.at[2]) * 0.5,
                ]),
                (_, [p, ..]) => Some(p.at),
                _ => None,
            };
            out.push(Element {
                id: e.id.clone(),
                kind,
                subkind: self.str(e.sub).map(String::from),
                level: self.level_name(e.level),
                at,
                tags: e
                    .tags
                    .iter()
                    .filter_map(|&t| self.str(t).map(String::from))
                    .collect(),
                props: json(&e.props),
                access_groups: if e.access != 0 {
                    self.access_names(e.access)
                } else {
                    vec![]
                },
                points,
            });
        }
        out
    }

    fn poi_elements(&self) -> Vec<Element> {
        self.pois
            .iter()
            .map(|p| {
                let mut points: Vec<ElementPoint> =
                    p.nodes.iter().map(|&n| self.node_point(n)).collect();
                for sn in &p.snaps {
                    let f = &self.facts[sn.fact as usize];
                    points.push(ElementPoint {
                        node: None,
                        at: sn.p,
                        level: self.level_name(f.level),
                        space: None,
                        edge: (f.elem != NONE).then(|| self.elems[f.elem as usize].id.clone()),
                    });
                }
                Element {
                    id: p.id.clone(),
                    kind: ElementKind::Poi,
                    subkind: p.category.clone(),
                    level: points.first().and_then(|x| x.level.clone()),
                    at: points.first().map(|x| x.at),
                    tags: p.tags.clone(),
                    props: json(&p.props),
                    access_groups: if p.access != 0 {
                        self.access_names(p.access)
                    } else {
                        vec![]
                    },
                    points,
                }
            })
            .collect()
    }
}
