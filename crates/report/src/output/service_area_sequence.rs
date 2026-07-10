use std::path::Path;

use anyhow::{Context, Result};
use netweevil_query::ServiceAreaSequenceResult;
use serde_json::{Map, Value, json};

use super::write_json;

pub(super) fn write_service_area_sequence_geojson(
    path: &Path,
    result: &ServiceAreaSequenceResult,
) -> Result<()> {
    let mut features = Vec::new();
    for frame in &result.frames {
        for feature in &frame.result.features {
            let mut properties = match serde_json::to_value(feature)
                .context("serializing service-area frame properties")?
            {
                Value::Object(properties) => properties,
                _ => Map::new(),
            };
            properties.remove("geometry");
            properties.insert(
                "sequence_id".to_string(),
                Value::String(result.sequence_id.clone()),
            );
            properties.insert("frame_index".to_string(), Value::from(frame.frame_index));
            properties.insert(
                "departure_time".to_string(),
                Value::String(frame.departure_time.clone()),
            );
            properties.insert(
                "scenario_id".to_string(),
                frame
                    .result
                    .scenario_id
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            properties.insert(
                "analysis_id".to_string(),
                Value::String(frame.result.analysis_id.clone()),
            );
            features.push(json!({
                "type": "Feature",
                "geometry": feature.geometry.clone().unwrap_or(Value::Null),
                "properties": properties,
            }));
        }
    }
    write_json(
        path,
        &json!({
            "type": "FeatureCollection",
            "features": features,
            "metadata": {
                "sequence_id": result.sequence_id,
                "frame_count": result.frame_count,
                "departure_times": result.frames.iter().map(|frame| &frame.departure_time).collect::<Vec<_>>(),
            }
        }),
    )
}
