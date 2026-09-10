//! Long-running workspace operations (imports, profile compilation, runtime
//! activation, scenario builds) run on blocking threads and report progress
//! through this registry so the console can poll them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use netweevil_manifest::now_rfc3339;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobRecord {
    pub(crate) job_id: String,
    pub(crate) kind: String,
    pub(crate) label: String,
    pub(crate) state: JobState,
    pub(crate) stage: String,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) percent: Option<f64>,
    pub(crate) started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
}

#[derive(Default)]
pub(crate) struct JobRegistry {
    jobs: Mutex<BTreeMap<String, JobRecord>>,
    counter: AtomicU64,
}

/// Progress handle passed into the blocking closure of a job.
#[derive(Clone)]
pub(crate) struct JobHandle {
    registry: Arc<JobRegistryRef>,
    pub(crate) job_id: String,
}

/// The registry lives inside the shared workspace; jobs hold a pointer to it
/// through the workspace so the handle stays `'static` for blocking tasks.
pub(crate) struct JobRegistryRef(pub(crate) Arc<crate::state::Workspace>);

impl JobHandle {
    pub(crate) fn progress(&self, stage: &str, message: &str, percent: Option<f64>) {
        self.registry.0.jobs.update(&self.job_id, |job| {
            job.stage = stage.to_string();
            job.message = message.to_string();
            job.percent = percent;
        });
    }
}

impl JobRegistry {
    pub(crate) fn start(&self, kind: &str, label: &str) -> String {
        let number = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let job_id = format!("{kind}-{number}");
        let record = JobRecord {
            job_id: job_id.clone(),
            kind: kind.to_string(),
            label: label.to_string(),
            state: JobState::Running,
            stage: "Queued".to_string(),
            message: String::new(),
            percent: None,
            started_at: now_rfc3339().unwrap_or_default(),
            finished_at: None,
            error: None,
            result: None,
        };
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id.clone(), record);
        job_id
    }

    pub(crate) fn update(&self, job_id: &str, apply: impl FnOnce(&mut JobRecord)) {
        if let Some(job) = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(job_id)
        {
            apply(job);
        }
    }

    pub(crate) fn finish(&self, job_id: &str, outcome: Result<Value, String>) {
        self.update(job_id, |job| {
            job.finished_at = Some(now_rfc3339().unwrap_or_default());
            job.percent = Some(100.0);
            match outcome {
                Ok(result) => {
                    job.state = JobState::Done;
                    job.stage = "Complete".to_string();
                    job.result = Some(result);
                }
                Err(error) => {
                    job.state = JobState::Failed;
                    job.stage = "Failed".to_string();
                    job.message = error.clone();
                    job.error = Some(error);
                }
            }
        });
    }

    pub(crate) fn get(&self, job_id: &str) -> Option<JobRecord> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job_id)
            .cloned()
    }

    pub(crate) fn list(&self) -> Vec<JobRecord> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        jobs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        jobs
    }

    /// Whether a job of `kind` is still running; used to serialise imports
    /// and activations that would otherwise race on the same files.
    pub(crate) fn is_running(&self, kind: &str) -> bool {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|job| job.kind == kind && job.state == JobState::Running)
    }
}

/// Runs `work` on a blocking thread as a tracked job and returns its id at once.
pub(crate) fn spawn_job<F>(
    workspace: Arc<crate::state::Workspace>,
    kind: &str,
    label: &str,
    work: F,
) -> String
where
    F: FnOnce(JobHandle) -> anyhow::Result<Value> + Send + 'static,
{
    let job_id = workspace.jobs.start(kind, label);
    let handle = JobHandle {
        registry: Arc::new(JobRegistryRef(Arc::clone(&workspace))),
        job_id: job_id.clone(),
    };
    let id = job_id.clone();
    tokio::task::spawn_blocking(move || {
        let outcome = work(handle).map_err(|error| format!("{error:#}"));
        workspace.jobs.finish(&id, outcome);
    });
    job_id
}
