#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileCompileStage {
    CompileEdgeMetrics,
    CompileAcceleration,
    Complete,
}

impl ProfileCompileStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::CompileEdgeMetrics => "Compile Edge Metrics",
            Self::CompileAcceleration => "Compile Acceleration",
            Self::Complete => "Complete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProfileCompileProgress {
    pub stage: ProfileCompileStage,
    pub stage_percent: Option<f64>,
    pub message: String,
}

pub(crate) fn emit_compile_progress(
    progress: &mut impl FnMut(ProfileCompileProgress),
    stage: ProfileCompileStage,
    stage_percent: Option<f64>,
    message: String,
) {
    progress(ProfileCompileProgress {
        stage,
        stage_percent,
        message,
    });
}

pub(crate) struct PercentReporter {
    last_bucket: Option<u32>,
}

impl PercentReporter {
    pub(crate) fn starting_at_zero() -> Self {
        Self {
            last_bucket: Some(0),
        }
    }

    pub(crate) fn emit_if_needed<F>(
        &mut self,
        completed: u64,
        total: u64,
        stage: ProfileCompileStage,
        progress: &mut impl FnMut(ProfileCompileProgress),
        message: F,
    ) where
        F: FnOnce(f64) -> String,
    {
        if total == 0 {
            return;
        }
        let percent = ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0);
        if percent >= 100.0 {
            return;
        }
        let bucket = (percent / 5.0).floor() as u32;
        if self.last_bucket == Some(bucket) {
            return;
        }
        self.last_bucket = Some(bucket);
        let quantized_percent = (bucket * 5) as f64;
        emit_compile_progress(
            progress,
            stage,
            Some(quantized_percent),
            message(quantized_percent),
        );
    }
}
