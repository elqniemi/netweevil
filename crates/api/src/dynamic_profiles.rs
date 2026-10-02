//! In-memory, request-defined street profiles. Named-profile requests never
//! enter this cache or acquire its locks. The cache belongs to one immutable
//! dataset runtime, so hashes cannot cross topology/acceleration versions.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use netweevil_profile::{ProfileDocument, compile_profile_bundle_with_acceleration};
use netweevil_query::PreparedRoutingEngine;
use serde_json::Value;
use tokio::sync::{Semaphore, oneshot};
use tracing::info;

use crate::error::ApiError;
use crate::state::{ServiceRuntime, resolve_profile};

pub(crate) struct DynamicProfileCache {
    capacity: usize,
    entries: Mutex<VecDeque<(String, Arc<PreparedRoutingEngine>)>>,
    compile_slot: Arc<Semaphore>,
    compiler: OnceLock<rayon::ThreadPool>,
    pending: Mutex<HashMap<String, Vec<oneshot::Sender<CompileResult>>>>,
}

type CompileResult = Result<(Arc<PreparedRoutingEngine>, bool), ApiError>;

impl DynamicProfileCache {
    pub(crate) fn from_env() -> Result<Self> {
        let capacity = match std::env::var("NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES") {
            Ok(value) => value.parse::<usize>().context(
                "NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES must be an integer from 1 to 16",
            )?,
            Err(std::env::VarError::NotPresent) => 2,
            Err(error) => return Err(error.into()),
        };
        if !(1..=16).contains(&capacity) {
            bail!("NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES must be from 1 to 16");
        }
        Ok(Self::new(capacity))
    }

    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(VecDeque::new()),
            compile_slot: Arc::new(Semaphore::new(1)),
            compiler: OnceLock::new(),
            pending: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, hash: &str) -> Option<Arc<PreparedRoutingEngine>> {
        let mut entries = self.entries.lock().expect("dynamic profile cache poisoned");
        let index = entries.iter().position(|(key, _)| key == hash)?;
        let entry = entries.remove(index).expect("cache entry exists");
        let engine = Arc::clone(&entry.1);
        entries.push_back(entry);
        Some(engine)
    }

    fn insert(&self, hash: String, engine: Arc<PreparedRoutingEngine>) {
        let evicted = {
            let mut entries = self.entries.lock().expect("dynamic profile cache poisoned");
            let evicted = if entries.len() == self.capacity {
                entries.pop_front()
            } else {
                None
            };
            entries.push_back((hash, engine));
            evicted
        };
        // Free potentially large engines outside the cache lock. Active
        // routes keep their own Arc and remain valid after eviction.
        drop(evicted);
    }
}

pub(crate) struct ResolvedDynamicProfile {
    pub(crate) engine: Arc<PreparedRoutingEngine>,
    pub(crate) cache_status: &'static str,
    /// Includes waiting for the compilation slot on a cold request.
    pub(crate) prepare_ms: f64,
}

pub(crate) async fn resolve_dynamic_profile(
    service: Arc<ServiceRuntime>,
    profile_id: Option<&str>,
    inline: Option<Value>,
    overrides: Option<Value>,
) -> Result<ResolvedDynamicProfile, ApiError> {
    let started = Instant::now();
    if inline.is_some() && (profile_id.is_some() || overrides.is_some()) {
        return Err(ApiError::bad_request(
            "profile cannot be combined with profile_id or profile_overrides",
        ));
    }
    let raw = if let Some(inline) = inline {
        inline
    } else {
        let base = resolve_profile(&service, profile_id)?;
        let mut raw = serde_json::to_value(&base.document)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        let overrides =
            overrides.ok_or_else(|| ApiError::bad_request("missing profile_overrides"))?;
        if !overrides.is_object() {
            return Err(ApiError::bad_request("profile_overrides must be an object"));
        }
        merge_overrides(&mut raw, overrides);
        raw
    };
    let document = parse_document(raw).map_err(ApiError::from_execution_error)?;
    let hash = document
        .fingerprint()
        .map_err(ApiError::from_execution_error)?;

    // An empty or equivalent override can reuse an already prepared profile.
    if let Some(profile) = service
        .profiles
        .values()
        .find(|p| p.manifest.profile_hash == hash)
    {
        return Ok(ResolvedDynamicProfile {
            engine: Arc::clone(&profile.engine),
            cache_status: "preloaded",
            prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }
    if let Some(engine) = service.dynamic_profiles.get(&hash) {
        return Ok(ResolvedDynamicProfile {
            engine,
            cache_status: "hit",
            prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }

    let (sender, receiver) = oneshot::channel();
    let leader = {
        let mut pending = service
            .dynamic_profiles
            .pending
            .lock()
            .expect("pending profiles poisoned");
        let waiters = pending.entry(hash.clone()).or_default();
        let leader = waiters.is_empty();
        waiters.push(sender);
        leader
    };
    if leader {
        // The task owns compilation independently of any one HTTP waiter.
        // Deliver the same Arc to every waiter even if another profile has
        // evicted this one before a waiter gets scheduled again.
        tokio::spawn(async move {
            let result = compile_queued_profile(service.clone(), document, hash.clone()).await;
            let waiters = service
                .dynamic_profiles
                .pending
                .lock()
                .expect("pending profiles poisoned")
                .remove(&hash)
                .unwrap_or_default();
            for waiter in waiters {
                let _ = waiter.send(result.clone());
            }
        });
    }
    let (engine, compiled) = receiver
        .await
        .map_err(|error| ApiError::internal(format!("profile compiler stopped: {error}")))??;
    Ok(ResolvedDynamicProfile {
        engine,
        cache_status: if leader && compiled { "miss" } else { "hit" },
        prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

async fn compile_queued_profile(
    service: Arc<ServiceRuntime>,
    document: ProfileDocument,
    hash: String,
) -> CompileResult {
    let permit = service
        .dynamic_profiles
        .compile_slot
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    tokio::task::spawn_blocking(move || {
        // Keep the permit and cache insertion inside the job: cancelling an
        // HTTP waiter must not release the slot while compilation still runs.
        let _permit = permit;
        let cache = &service.dynamic_profiles;
        if let Some(engine) = cache.get(&hash) {
            return Ok((engine, false));
        }
        if cache.compiler.get().is_none() {
            let compiler = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "profile-compiler".to_string())
                .build()
                .map_err(|error| ApiError::internal(error.to_string()))?;
            let _ = cache.compiler.set(compiler);
        }
        let compile_started = Instant::now();
        let engine = cache
            .compiler
            .get()
            .expect("compiler initialized")
            .install(|| compile_engine(&service, &document))
            .map_err(ApiError::from_execution_error)?;
        let engine = Arc::new(engine);
        cache.insert(hash.clone(), Arc::clone(&engine));
        info!(profile_hash = %hash,
            compile_ms = compile_started.elapsed().as_secs_f64() * 1000.0,
            cache_capacity = cache.capacity, "request profile prepared");
        Ok((engine, true))
    })
    .await
    .map_err(|error| ApiError::internal(format!("profile compiler failed: {error}")))?
}

fn compile_engine(
    service: &ServiceRuntime,
    document: &ProfileDocument,
) -> Result<PreparedRoutingEngine> {
    let topology_ref = service
        .dataset_manifest
        .topology_bundle
        .as_ref()
        .context("dataset is missing its topology bundle")?;
    let acceleration = service.profiles[&service.default_profile_id]
        .dataset_acceleration
        .clone();
    let metrics = compile_profile_bundle_with_acceleration(
        document,
        &service.topology,
        topology_ref.bundle_id.clone(),
        service
            .dataset_manifest
            .acceleration_bundle
            .as_ref()
            .zip(acceleration.as_ref())
            .map(|(reference, bundle)| (bundle.as_ref(), reference.bundle_id.clone())),
    )?;
    // No manifests or bundles are written for request-defined profiles.
    PreparedRoutingEngine::new(
        Arc::clone(&service.topology),
        Arc::new(metrics),
        acceleration,
    )
}

/// Objects merge recursively; arrays and scalar values replace the old value.
/// Null is a literal value, useful for clearing an optional slope model.
fn merge_overrides(base: &mut Value, overrides: Value) {
    match (base, overrides) {
        (Value::Object(base), Value::Object(overrides)) => {
            for (key, value) in overrides {
                merge_overrides(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, value) => *base = value,
    }
}

fn parse_document(raw: Value) -> Result<ProfileDocument> {
    let document: ProfileDocument =
        serde_json::from_value(raw.clone()).context("invalid profile")?;
    // Reject misspelled override fields before compiling the profile.
    reject_unknown_fields(&raw, &serde_json::to_value(&document)?, "profile")?;
    document.validate()?;
    for value in [
        document.turns.left_penalty_s,
        document.turns.right_penalty_s,
        document.turns.uturn_penalty_s,
        document.turns.traffic_signal_penalty_s,
        document.turns.roundabout_entry_penalty_s,
        document.preferences.service_penalty_s,
        document.ferry.boarding_cost_s,
    ] {
        if !value.is_finite() || value < 0.0 {
            bail!("profile penalties must be finite and non-negative");
        }
    }
    Ok(document)
}

fn reject_unknown_fields(raw: &Value, normalized: &Value, path: &str) -> Result<()> {
    match (raw, normalized) {
        (Value::Object(raw), Value::Object(normalized)) => {
            for (key, value) in raw {
                let path = format!("{path}.{key}");
                let expected = normalized
                    .get(key)
                    .with_context(|| format!("unknown field {path}"))?;
                reject_unknown_fields(value, expected, &path)?;
            }
        }
        (Value::Array(raw), Value::Array(normalized)) => {
            for (index, (value, expected)) in raw.iter().zip(normalized).enumerate() {
                reject_unknown_fields(value, expected, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
