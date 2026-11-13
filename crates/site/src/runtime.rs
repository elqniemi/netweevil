//! Loaded sites: the compiled graph behind a shared [`Router`], the input
//! for display, and a pool of query scratch buffers. [`SiteCatalog`] loads
//! sites on first use and reloads one when its manifest changes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result, bail};
use netweevil_core::LocalFrame;
use netweevil_indoor::{Input, QueryCtx, Router, load_graph, parse_input};
use netweevil_persist::WorkspacePaths;
use netweevil_persist::indoor::{
    IndoorSiteManifest, read_indoor_site_manifest, read_indoor_site_manifests,
};

use crate::store::ring_contains;

/// Query scratch buffers kept per site.
const CTX_POOL: usize = 16;

/// One site ready for queries. Share it across threads.
pub struct IndoorSite {
    pub manifest: IndoorSiteManifest,
    pub input: Input,
    pub router: Router,
    ctx_pool: Mutex<Vec<QueryCtx>>,
}

impl IndoorSite {
    /// Loads a site's input and compiled graph from its manifest.
    pub fn load(manifest: IndoorSiteManifest) -> Result<Self> {
        let text = std::fs::read_to_string(&manifest.input_path)
            .with_context(|| format!("reading {}", manifest.input_path))?;
        let input = parse_input(&text)?;
        let graph = load_graph(Path::new(&manifest.graph_path)).with_context(|| {
            format!(
                "reading indoor graph {}; import the site again",
                manifest.graph_path
            )
        })?;
        Ok(Self::new(manifest, input, Router::new(graph)))
    }

    pub fn new(manifest: IndoorSiteManifest, input: Input, router: Router) -> Self {
        Self {
            manifest,
            input,
            router,
            ctx_pool: Mutex::new(vec![]),
        }
    }

    pub fn site_id(&self) -> &str {
        &self.manifest.site_id
    }

    pub fn frame(&self) -> &LocalFrame {
        &self.manifest.anchor
    }

    /// Runs `f` with a query context from the pool.
    pub fn with_ctx<T>(&self, f: impl FnOnce(&mut QueryCtx) -> T) -> T {
        let ctx = self
            .ctx_pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let mut ctx = ctx.unwrap_or_else(|| self.router.ctx());
        let out = f(&mut ctx);
        let mut pool = self.ctx_pool.lock().unwrap_or_else(PoisonError::into_inner);
        if pool.len() < CTX_POOL {
            pool.push(ctx);
        }
        out
    }

    /// Whether `[lon, lat]` lies inside the site footprint.
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        ring_contains(&self.manifest.footprint, [lon, lat])
    }
}

/// Sites of a workspace, loaded on first use and shared.
pub struct SiteCatalog {
    paths: WorkspacePaths,
    loaded: Mutex<HashMap<String, Arc<IndoorSite>>>,
}

impl SiteCatalog {
    pub fn new(paths: WorkspacePaths) -> Self {
        Self {
            paths,
            loaded: Mutex::new(HashMap::new()),
        }
    }

    pub fn paths(&self) -> &WorkspacePaths {
        &self.paths
    }

    /// Every site manifest, sorted by id.
    pub fn manifests(&self) -> Result<Vec<IndoorSiteManifest>> {
        read_indoor_site_manifests(&self.paths)
    }

    /// A loaded site. A cached site is reused while its manifest is unchanged.
    pub fn get(&self, site_id: &str) -> Result<Arc<IndoorSite>> {
        let manifest = read_indoor_site_manifest(&self.paths, site_id)?;
        if let Some(site) = self
            .loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(site_id)
            && site.manifest.created_at == manifest.created_at
        {
            return Ok(site.clone());
        }
        let site = Arc::new(IndoorSite::load(manifest)?);
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(site_id.to_string(), site.clone());
        Ok(site)
    }

    /// The site whose footprint contains `[lon, lat]` (the smallest when
    /// several do).
    pub fn site_at(&self, lon: f64, lat: f64) -> Result<Arc<IndoorSite>> {
        let mut best: Option<(f64, String)> = None;
        for m in self.manifests()? {
            if ring_contains(&m.footprint, [lon, lat]) {
                let area = (m.bbox[2] - m.bbox[0]) * (m.bbox[3] - m.bbox[1]);
                if best.as_ref().is_none_or(|b| area < b.0) {
                    best = Some((area, m.site_id));
                }
            }
        }
        match best {
            Some((_, id)) => self.get(&id),
            None => bail!("no indoor site contains ({lon}, {lat}); give site_id"),
        }
    }

    /// Drops a cached site (after it was removed or re-imported).
    pub fn forget(&self, site_id: &str) {
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(site_id);
    }
}
