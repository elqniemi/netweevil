use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::Duration;
use time::format_description::well_known::Rfc3339;

use crate::{
    RoutingGraph, ServiceAreaRequest, ServiceAreaResult, build_temporal_routing_graph,
    execute_service_area_with_graph, parse_datetime,
};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};

/// Replays one service-area request at a sequence of departure datetimes.
///
/// Callers may either provide `departure_times` explicitly or an inclusive
/// `start_time`/`end_time` range with `step_s`. Keeping the spatial request in
/// one template makes boundary-time isochrone animations reproducible without
/// duplicating every origin, threshold, and return option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaSequenceRequest {
    pub sequence_id: String,
    pub request: ServiceAreaRequest,
    #[serde(default)]
    pub departure_times: Vec<String>,
    #[serde(default)]
    pub start_time: Option<String>,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub step_s: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaFrameResult {
    pub frame_index: u32,
    pub departure_time: String,
    pub result: ServiceAreaResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaSequenceResult {
    pub sequence_id: String,
    pub frame_count: usize,
    pub frames: Vec<ServiceAreaFrameResult>,
}

pub(crate) fn execute_service_area_sequence_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &ServiceAreaSequenceRequest,
) -> Result<ServiceAreaSequenceResult> {
    let departure_times = request.expanded_departure_times()?;
    let temporal_routing_graph;
    let routing_graph = if routing_graph.includes_temporal_materialized_directions {
        routing_graph
    } else {
        temporal_routing_graph = build_temporal_routing_graph(topology, metrics)?;
        &temporal_routing_graph
    };
    let mut frames = Vec::with_capacity(departure_times.len());
    for (frame_index, departure_time) in departure_times.into_iter().enumerate() {
        let mut frame_request = request.request.clone();
        frame_request.temporal.departure_time = Some(departure_time.clone());
        let result =
            execute_service_area_with_graph(topology, metrics, routing_graph, &frame_request)
                .with_context(|| {
                    format!(
                        "executing service-area sequence '{}' frame {} at {}",
                        request.sequence_id, frame_index, departure_time
                    )
                })?;
        frames.push(ServiceAreaFrameResult {
            frame_index: u32::try_from(frame_index)
                .context("service-area sequence frame index exceeds u32")?,
            departure_time,
            result,
        });
    }
    Ok(ServiceAreaSequenceResult {
        sequence_id: request.sequence_id.clone(),
        frame_count: frames.len(),
        frames,
    })
}

impl ServiceAreaSequenceRequest {
    pub fn expanded_departure_times(&self) -> Result<Vec<String>> {
        if self.sequence_id.trim().is_empty() {
            bail!("service-area sequence_id must not be empty");
        }
        if self.request.temporal.departure_time.is_some()
            || self.request.temporal.arrive_by.is_some()
        {
            bail!(
                "service-area sequence request template must not set departure_time or arrive_by; use the sequence timing fields"
            );
        }

        let has_explicit = !self.departure_times.is_empty();
        let has_range =
            self.start_time.is_some() || self.end_time.is_some() || self.step_s.is_some();
        if has_explicit && has_range {
            bail!(
                "service-area sequence must use either departure_times or start_time/end_time/step_s, not both"
            );
        }
        if has_explicit {
            if self.departure_times.len() > 10_000 {
                bail!("service-area sequence is limited to 10000 frames");
            }
            for value in &self.departure_times {
                parse_datetime(value)
                    .with_context(|| format!("invalid sequence departure_time '{value}'"))?;
            }
            return Ok(self.departure_times.clone());
        }

        let start_text = self
            .start_time
            .as_deref()
            .context("service-area sequence requires departure_times or start_time")?;
        let end_text = self
            .end_time
            .as_deref()
            .context("service-area sequence range requires end_time")?;
        let step_s = self
            .step_s
            .context("service-area sequence range requires step_s")?;
        if step_s == 0 {
            bail!("service-area sequence step_s must be greater than zero");
        }
        let start =
            parse_datetime(start_text).context("invalid service-area sequence start_time")?;
        let end = parse_datetime(end_text).context("invalid service-area sequence end_time")?;
        if end < start {
            bail!("service-area sequence end_time must not precede start_time");
        }
        let step = Duration::seconds(
            i64::try_from(step_s).context("service-area sequence step_s is too large")?,
        );
        let mut values = Vec::new();
        let mut current = start;
        while current <= end {
            if values.len() >= 10_000 {
                bail!("service-area sequence is limited to 10000 frames");
            }
            values.push(
                current
                    .format(&Rfc3339)
                    .context("formatting service-area frame datetime")?,
            );
            current = current
                .checked_add(step)
                .context("service-area sequence datetime overflow")?;
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ServiceAreaSequenceRequest {
        ServiceAreaSequenceRequest {
            sequence_id: "flip".to_string(),
            request: ServiceAreaRequest {
                analysis_id: "area".to_string(),
                origins: Vec::new(),
                thresholds: Vec::new(),
                snap: Default::default(),
                connectivity: Default::default(),
                boundary_mode: Default::default(),
                output_mode: Default::default(),
                band_mode: Default::default(),
                multi_origin_mode: Default::default(),
                polygon: Default::default(),
                fallback: Default::default(),
                returns: Default::default(),
                temporal: Default::default(),
            },
            departure_times: Vec::new(),
            start_time: Some("2026-07-10T09:40:00+08:00".to_string()),
            end_time: Some("2026-07-10T10:20:00+08:00".to_string()),
            step_s: Some(300),
        }
    }

    #[test]
    fn expands_inclusive_boundary_time_sequence() {
        let values = request().expanded_departure_times().expect("sequence");
        assert_eq!(values.len(), 9);
        assert_eq!(values.first().unwrap(), "2026-07-10T09:40:00+08:00");
        assert_eq!(values.last().unwrap(), "2026-07-10T10:20:00+08:00");
    }

    #[test]
    fn rejects_ambiguous_sequence_timing() {
        let mut request = request();
        request.departure_times = vec!["2026-07-10T10:00:00+08:00".to_string()];
        assert!(request.expanded_departure_times().is_err());
    }
}
