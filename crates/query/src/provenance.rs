use anyhow::Result;

use crate::TemporalRequestOptions;

#[derive(Debug, Clone, Default)]
pub(crate) struct TemporalResultProvenance {
    pub(crate) departure_time: Option<String>,
    pub(crate) arrive_by: Option<String>,
    pub(crate) scenario_id: Option<String>,
}

pub(crate) fn temporal_result_provenance(
    temporal: &TemporalRequestOptions,
) -> Result<TemporalResultProvenance> {
    let scenario_id = temporal
        .scenario
        .as_ref()
        .and_then(|scenario| scenario.id.clone());
    Ok(TemporalResultProvenance {
        departure_time: temporal.departure_time.clone(),
        arrive_by: temporal.arrive_by.clone(),
        scenario_id,
    })
}
