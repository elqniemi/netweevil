//! Display context from stops.txt without loading a feed's timetable again.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GtfsStopContext {
    pub stop_name: String,
    pub station_name: Option<String>,
    pub platform_code: Option<String>,
}

pub fn read_gtfs_stop_context(path: impl AsRef<Path>) -> Result<BTreeMap<String, GtfsStopContext>> {
    let path = path.as_ref();
    let text = if path.is_dir() {
        std::fs::read_to_string(path.join("stops.txt"))?
    } else {
        let mut archive = zip::ZipArchive::new(File::open(path)?)?;
        let mut text = String::new();
        archive.by_name("stops.txt")?.read_to_string(&mut text)?;
        text
    };
    parse_stop_context(&text)
}

fn parse_stop_context(text: &str) -> Result<BTreeMap<String, GtfsStopContext>> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let column = |name: &str| headers.iter().position(|header| header == name);
    let id = column("stop_id").context("stops.txt is missing stop_id")?;
    let name = column("stop_name").context("stops.txt is missing stop_name")?;
    let parent = column("parent_station");
    let platform = column("platform_code");
    let mut rows = BTreeMap::new();
    for row in reader.records() {
        let row = row?;
        let optional = |column: Option<usize>| {
            column
                .and_then(|i| row.get(i))
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        rows.insert(
            row.get(id).unwrap_or_default().to_string(),
            (
                row.get(name).unwrap_or_default().to_string(),
                optional(parent),
                optional(platform),
            ),
        );
    }
    Ok(rows
        .iter()
        .map(|(id, (name, parent, platform))| {
            (
                id.clone(),
                GtfsStopContext {
                    stop_name: name.clone(),
                    station_name: parent
                        .as_ref()
                        .and_then(|id| rows.get(id))
                        .map(|row| row.0.clone()),
                    platform_code: platform.clone(),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_parent_station_and_platform_without_requiring_timetable_files() {
        let rows = parse_stop_context("stop_id,stop_name,parent_station,platform_code\nS,Central,,\nP,Platform 1,S,1\nB,Bus stop,,\n").unwrap();
        assert_eq!(rows["P"].station_name.as_deref(), Some("Central"));
        assert_eq!(rows["P"].platform_code.as_deref(), Some("1"));
        assert_eq!(rows["B"].station_name, None);
    }
}
