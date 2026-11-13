//! Routing profiles and the built-ins `walk`, `wheelchair`, `fastest`, `bike`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `null` (None) = forbidden.
pub type FactorMap = BTreeMap<String, Option<f64>>;

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Corner {
    #[default]
    Round,
    Square,
    Sharp,
}

/// Which kinds of elements a route may travel on.
///
/// The mode is part of the profile, so each mode compiles to its own cached costs. The
/// excluded elements get an infinite factor, the same as a `null` class factor, so the
/// default mode adds no work to a search.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    /// Walks across surfaces, edges, portals and verticals.
    #[default]
    Mixed,
    /// Only edges, portals, verticals and nodes: a route never walks across a surface. A point
    /// locator snaps to the nearest usable edge, and space locators fail.
    EdgesOnly,
    /// Only surfaces, portals, verticals and nodes: a route never travels along an edge. A point
    /// locator snaps to the nearest usable surface.
    SurfacesOnly,
}

impl RouteMode {
    pub fn name(self) -> &'static str {
        match self {
            RouteMode::Mixed => "mixed",
            RouteMode::EdgesOnly => "edges_only",
            RouteMode::SurfacesOnly => "surfaces_only",
        }
    }

    pub fn is_mixed(&self) -> bool {
        *self == RouteMode::Mixed
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct Incline {
    pub up: f64,
    pub down: f64,
    pub max: Option<f64>,
}

impl Default for Incline {
    fn default() -> Self {
        Incline {
            up: 0.0,
            down: 0.0,
            max: None,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub speed: f64,
    pub class_factor: FactorMap,
    pub tag_factor: FactorMap,
    pub avoid_tags: Vec<String>,
    pub require_tags: Vec<String>,
    pub step_free: bool,
    pub min_width: Option<f64>,
    pub incline: Incline,
    pub kind_penalty: BTreeMap<String, f64>,
    /// Seconds of preference cost added once per traversal of an element carrying the tag
    /// (portal, vertical hop, node, edge — split over its pieces by length; not surfaces).
    /// Not part of the route duration.
    pub tag_penalty: BTreeMap<String, f64>,
    /// Speed cap (m/s) for elements carrying the tag, class or kind (`{"indoor": 1.3}`: the bike
    /// is left outside). The lowest matching cap wins; an element's own `speed` still applies.
    pub tag_speed: BTreeMap<String, f64>,
    pub kind_factor: FactorMap,
    /// Direction-specific factors keyed by vertical kind or edge class (e.g. `stairs`), used
    /// instead of `kind_factor` (verticals) / `class_factor` (edges) when going up / down.
    pub kind_factor_up: FactorMap,
    pub kind_factor_down: FactorMap,
    pub turn_penalty: f64,
    /// Minimum distance kept from walls and corners when drawing walks (m).
    pub clearance: f64,
    /// Cap of the width-adaptive clearance: walks keep 40% of the local free width from
    /// walls (corridors: near the centreline), at least `clearance`, at most this (m).
    pub clearance_max: f64,
    pub corner: Corner,
    /// Minimum turn radius of `round` corners (m).
    pub corner_radius: f64,
    /// Where this profile may not go, go on foot with this profile instead (a built-in name or
    /// a profile object; its own `on_foot` is ignored). Bike: `"walk"`, so buildings, stairs
    /// and ways closed to bikes are walked (pushing the bike outdoors). An element is walked
    /// exactly when this profile forbids it and the on-foot profile allows it; route segments
    /// then carry `mode` (`ride` / `walk`) and the route its `legs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_foot: Option<OnFoot>,
    /// Walked elements carrying one of these tags (or classes) are walked without the bike,
    /// which is left at their edge: a route uses them only in the connected area of such
    /// elements its start or end lies in (bike: `["indoor"]`, so a ride never crosses a
    /// building on foot; it starts at the start building's door and ends at the destination's).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub leave_tags: Vec<String>,
    /// Element kinds the route may use (`mixed`, `edges_only`, `surfaces_only`). The on-foot
    /// profile follows the mode of this profile.
    #[serde(skip_serializing_if = "RouteMode::is_mixed")]
    pub mode: RouteMode,
}

/// `on_foot`: a built-in profile name or a profile object.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum OnFoot {
    Name(String),
    Profile(Box<Profile>),
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "custom".into(),
            speed: 1.3,
            class_factor: FactorMap::new(),
            tag_factor: FactorMap::new(),
            avoid_tags: vec![],
            require_tags: vec![],
            step_free: false,
            min_width: None,
            incline: Incline::default(),
            kind_penalty: BTreeMap::new(),
            tag_penalty: BTreeMap::new(),
            tag_speed: BTreeMap::new(),
            kind_factor: FactorMap::new(),
            kind_factor_up: FactorMap::new(),
            kind_factor_down: FactorMap::new(),
            turn_penalty: 0.0,
            clearance: 0.3,
            clearance_max: 1.0,
            corner: Corner::Round,
            corner_radius: 0.6,
            on_foot: None,
            leave_tags: vec![],
            mode: RouteMode::Mixed,
        }
    }
}

fn fm(items: &[(&str, Option<f64>)]) -> FactorMap {
    items.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn pm(items: &[(&str, f64)]) -> BTreeMap<String, f64> {
    items.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

pub const BUILTINS: [&str; 5] = ["walk", "wheelchair", "fastest", "bike", "evacuation"];

/// Emergency / service stairs, doors and corridors are used only when they save more than
/// this (seconds); main ones are slightly preferred.
fn role_penalty() -> BTreeMap<String, f64> {
    pm(&[("role:emergency", 120.0), ("role:service", 60.0)])
}

impl Profile {
    pub fn builtin(name: &str) -> Option<Profile> {
        let p = match name {
            "walk" => Profile {
                name: "walk".into(),
                speed: 1.3,
                class_factor: fm(&[("room", Some(1.6)), ("service", Some(2.5))]),
                incline: Incline {
                    up: 2.0,
                    down: 0.5,
                    max: None,
                },
                kind_penalty: pm(&[("door", 2.0), ("gate", 2.0), ("lift", 0.0)]),
                kind_factor: fm(&[
                    ("stairs", Some(1.0)),
                    ("escalator", Some(1.0)),
                    ("lift", Some(1.0)),
                ]),
                // Climbing stairs costs more than descending them.
                kind_factor_up: fm(&[("stairs", Some(2.2))]),
                kind_factor_down: fm(&[("stairs", Some(1.6))]),
                tag_factor: fm(&[("role:main", Some(0.95))]),
                tag_penalty: role_penalty(),
                clearance: 0.3,
                corner: Corner::Round,
                corner_radius: 0.6,
                ..Profile::default()
            },
            "wheelchair" => Profile {
                name: "wheelchair".into(),
                speed: 1.0,
                class_factor: fm(&[("room", Some(1.6)), ("service", Some(2.5))]),
                avoid_tags: vec!["stairs".into(), "steps".into(), "escalator".into()],
                step_free: true,
                min_width: Some(0.85),
                incline: Incline {
                    up: 8.0,
                    down: 2.0,
                    max: Some(0.08),
                },
                kind_penalty: pm(&[("door", 4.0), ("gate", 4.0)]),
                kind_factor: fm(&[("stairs", None), ("escalator", None), ("lift", Some(1.0))]),
                tag_factor: fm(&[("role:main", Some(0.95))]),
                tag_penalty: role_penalty(),
                clearance: 0.45,
                clearance_max: 1.2,
                corner: Corner::Round,
                corner_radius: 0.8,
                ..Profile::default()
            },
            "fastest" => Profile {
                name: "fastest".into(),
                speed: 1.4,
                incline: Incline {
                    up: 1.0,
                    down: 0.0,
                    max: None,
                },
                kind_penalty: pm(&[("door", 1.0)]),
                tag_penalty: role_penalty(),
                clearance: 0.25,
                clearance_max: 0.6,
                corner: Corner::Round,
                corner_radius: 0.4,
                ..Profile::default()
            },
            // Bike: ride outdoors, walk inside. Everything the bike may not use is walked with
            // the `walk` profile (`on_foot`): indoor elements (tag `indoor`, for everything
            // off the outdoor level), ways closed to bikes (`no_bicycle`),
            // stairs, escalators and outdoor steps (carrying the bike).
            "bike" => Profile {
                name: "bike".into(),
                speed: 4.5,
                class_factor: fm(&[
                    ("footway", Some(2.0)),
                    ("pedestrian", Some(2.0)),
                    ("steps", None),
                ]),
                tag_factor: fm(&[
                    ("indoor", None),
                    ("no_bicycle", None),
                    ("bicycle:dismount", None),
                ]),
                tag_penalty: role_penalty(),
                incline: Incline {
                    up: 10.0,
                    down: 0.0,
                    max: None,
                },
                kind_penalty: pm(&[("door", 2.0), ("gate", 2.0), ("lift", 0.0)]),
                kind_factor: fm(&[("stairs", None), ("escalator", None), ("lift", Some(1.0))]),
                clearance: 0.5,
                clearance_max: 1.5,
                corner: Corner::Round,
                corner_radius: 2.0,
                on_foot: Some(OnFoot::Name("walk".into())),
                leave_tags: vec!["indoor".into()],
                ..Profile::default()
            },
            // Evacuation: walking without lifts or escalators; emergency stairs and exits are
            // preferred and carry no penalty, doors cost little.
            "evacuation" => Profile {
                name: "evacuation".into(),
                speed: 1.3,
                class_factor: fm(&[("room", Some(1.3)), ("service", Some(1.5))]),
                tag_factor: fm(&[("role:emergency", Some(0.8))]),
                incline: Incline {
                    up: 2.0,
                    down: 0.5,
                    max: None,
                },
                kind_penalty: pm(&[("door", 1.0), ("gate", 1.0)]),
                kind_factor: fm(&[("stairs", Some(1.0)), ("escalator", None), ("lift", None)]),
                kind_factor_up: fm(&[("stairs", Some(2.2))]),
                kind_factor_down: fm(&[("stairs", Some(1.6))]),
                clearance: 0.3,
                corner: Corner::Round,
                corner_radius: 0.6,
                ..Profile::default()
            },
            _ => return None,
        };
        Some(p)
    }

    /// Built-in name, a JSON object string, or a path to a JSON file.
    pub fn resolve(spec: &str) -> Result<Profile, String> {
        if let Some(p) = Profile::builtin(spec) {
            return Ok(p);
        }
        let text = if spec.trim_start().starts_with('{') {
            spec.to_string()
        } else {
            std::fs::read_to_string(spec).map_err(|e| format!("profile {spec}: {e}"))?
        };
        let p: Profile = serde_json::from_str(&text).map_err(|e| format!("profile: {e}"))?;
        p.validate()?;
        Ok(p)
    }

    /// The on-foot profile (`on_foot`), resolved; its own `on_foot` is dropped.
    pub fn foot(&self) -> Result<Option<Profile>, String> {
        let p = match &self.on_foot {
            None => return Ok(None),
            Some(OnFoot::Name(n)) => Profile::builtin(n)
                .ok_or_else(|| format!("profile {}: on_foot: unknown profile {n:?}", self.name))?,
            Some(OnFoot::Profile(p)) => (**p).clone(),
        };
        Ok(Some(Profile { on_foot: None, ..p }))
    }

    /// Reject values that would make costs negative or meaningless.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(f) = self.foot()? {
            f.validate()?;
        }
        let bad =
            |what: &str, v: f64| format!("profile {}: {what} must be >= 0 (got {v})", self.name);
        if !(self.speed.is_finite() && self.speed > 0.0) {
            return Err(format!(
                "profile {}: speed must be > 0 (got {})",
                self.name, self.speed
            ));
        }
        for (map, what) in [
            (&self.class_factor, "class_factor"),
            (&self.tag_factor, "tag_factor"),
            (&self.kind_factor, "kind_factor"),
            (&self.kind_factor_up, "kind_factor_up"),
            (&self.kind_factor_down, "kind_factor_down"),
        ] {
            for (k, v) in map {
                if let Some(v) = v
                    && !(v.is_finite() && *v >= 0.0)
                {
                    return Err(bad(&format!("{what}.{k}"), *v));
                }
            }
        }
        for (k, v) in &self.kind_penalty {
            if !(v.is_finite() && *v >= 0.0) {
                return Err(bad(&format!("kind_penalty.{k}"), *v));
            }
        }
        for (k, v) in &self.tag_penalty {
            if !(v.is_finite() && *v >= 0.0) {
                return Err(bad(&format!("tag_penalty.{k}"), *v));
            }
        }
        for (k, v) in &self.tag_speed {
            if !(v.is_finite() && *v > 0.0) {
                return Err(format!(
                    "profile {}: tag_speed.{k} must be > 0 (got {v})",
                    self.name
                ));
            }
        }
        for (what, v) in [
            ("incline.up", self.incline.up),
            ("incline.down", self.incline.down),
            ("incline.max", self.incline.max.unwrap_or(0.0)),
            ("turn_penalty", self.turn_penalty),
            ("clearance", self.clearance),
            ("clearance_max", self.clearance_max),
            ("corner_radius", self.corner_radius),
            ("min_width", self.min_width.unwrap_or(0.0)),
        ] {
            if !(v.is_finite() && v >= 0.0) {
                return Err(bad(what, v));
            }
        }
        Ok(())
    }
}
