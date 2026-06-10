use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::schema::ProfileDocument;

pub fn load_profile(path: impl AsRef<Path>) -> Result<ProfileDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading profile file {}", path.display()))?;
    let parsed = match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => toml::from_str(&raw).context("parsing TOML profile")?,
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw).context("parsing YAML profile")?,
        other => bail!(
            "unsupported profile extension {:?}; use .yml, .yaml, or .toml",
            other
        ),
    };
    Ok(parsed)
}
