//! Read-only discovery and serving of prepared local raster DEM tiles.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::state::ApiState;

#[derive(Debug, Deserialize, Serialize)]
struct TerrainSource {
    id: String,
    name: String,
    version: String,
    bounds: [f64; 4],
    minzoom: u8,
    maxzoom: u8,
    encoding: String,
    tile_size: u16,
    vertical_datum: String,
    resolution_m: f64,
    attribution: String,
    #[serde(default)]
    tiles: Vec<String>,
    #[serde(flatten)]
    provenance: BTreeMap<String, Value>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_version(version: &str) -> bool {
    (16..=64).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn contained_file(root: &Path, path: &Path) -> Result<PathBuf, ApiError> {
    let root = root
        .canonicalize()
        .map_err(|_| ApiError::not_found("terrain source not found"))?;
    let path = path
        .canonicalize()
        .map_err(|_| ApiError::not_found("terrain file not found"))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(ApiError::not_found("terrain file not found"));
    }
    Ok(path)
}

fn read_source(root: &Path, id: &str) -> Result<TerrainSource, ApiError> {
    if !valid_id(id) {
        return Err(ApiError::bad_request("invalid terrain source id"));
    }
    let path = contained_file(root, &root.join(id).join("manifest.json"))?;
    let data = fs::read(path).map_err(|_| ApiError::internal("cannot read terrain manifest"))?;
    let mut source: TerrainSource = serde_json::from_slice(&data)
        .map_err(|_| ApiError::internal("invalid terrain manifest"))?;
    let [west, south, east, north] = source.bounds;
    if source.id != id
        || source.name.trim().is_empty()
        || !valid_version(&source.version)
        || !source.bounds.iter().all(|v| v.is_finite())
        || west < -180.0
        || east > 180.0
        || west >= east
        || south < -90.0
        || north > 90.0
        || south >= north
        || source.minzoom > source.maxzoom
        || source.maxzoom > 24
        || !matches!(source.encoding.as_str(), "terrarium" | "mapbox")
        || !matches!(source.tile_size, 256 | 512)
        || !source.resolution_m.is_finite()
        || source.resolution_m <= 0.0
    {
        return Err(ApiError::internal("invalid terrain manifest metadata"));
    }
    // Clients always use this server, regardless of URLs in a local manifest.
    source.tiles = vec![format!(
        "/v1/terrain/{}/{}/{{z}}/{{x}}/{{y}}.png",
        source.id, source.version
    )];
    Ok(source)
}

fn discover(root: &Path) -> Result<Value, ApiError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"sources":[]}));
        }
        Err(_) => return Err(ApiError::internal("cannot list terrain sources")),
    };
    let mut sources = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if !valid_id(&id) || !entry.path().join("manifest.json").exists() {
            continue;
        }
        match read_source(root, &id) {
            Ok(source) => sources.push(source),
            Err(_) => diagnostics.push(format!(
                "Terrain source '{id}' has an unreadable or invalid manifest."
            )),
        }
    }
    sources.sort_by(|a, b| a.id.cmp(&b.id));
    diagnostics.sort();
    Ok(json!({"sources":sources,"diagnostics":diagnostics}))
}

pub(crate) async fn terrain_sources_handler(
    State(state): State<ApiState>,
) -> Result<Json<Value>, ApiError> {
    let root = state.workspace.paths.state_dir.join("terrain");
    tokio::task::spawn_blocking(move || discover(&root))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map(Json)
}

fn tile_response(
    root: &Path,
    id: &str,
    version: &str,
    z: u8,
    x: u32,
    tile: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let y = tile
        .strip_suffix(".png")
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| ApiError::bad_request("invalid terrain tile name"))?;
    if !valid_id(id) || !valid_version(version) || z > 24 || x >= (1u32 << z) || y >= (1u32 << z) {
        return Err(ApiError::bad_request(
            "invalid terrain tile coordinates or source",
        ));
    }
    let source = read_source(root, id)?;
    if z < source.minzoom || z > source.maxzoom {
        return Err(ApiError::not_found("terrain zoom unavailable"));
    }
    // Keep older immutable versions usable after a new manifest is published.
    let path = contained_file(
        root,
        &root
            .join(id)
            .join(version)
            .join("tiles")
            .join(z.to_string())
            .join(x.to_string())
            .join(format!("{y}.png")),
    )?;
    let etag = format!("\"{id}-{version}-{z}-{x}-{y}\"");
    let response_headers = [
        (header::CONTENT_TYPE, "image/png".to_owned()),
        (
            header::CACHE_CONTROL,
            "public, max-age=31536000, immutable".to_owned(),
        ),
        (header::ETAG, etag.clone()),
    ];
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|tag| tag.trim() == etag || tag.trim() == "*")
        })
    {
        return Ok((StatusCode::NOT_MODIFIED, response_headers).into_response());
    }
    let bytes = fs::read(path).map_err(|_| ApiError::internal("cannot read terrain tile"))?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ApiError::internal("invalid terrain PNG tile"));
    }
    Ok((response_headers, bytes).into_response())
}

pub(crate) async fn terrain_tile_handler(
    State(state): State<ApiState>,
    UrlPath((id, version, z, x, tile)): UrlPath<(String, String, u8, u32, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let root = state.workspace.paths.state_dir.join("terrain");
    tokio::task::spawn_blocking(move || tile_response(&root, &id, &version, z, x, &tile, &headers))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "netweevil-terrain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("hk/0123456789abcdef/tiles/8/209")).unwrap();
        fs::write(
            root.join("hk/manifest.json"),
            serde_json::to_vec(&json!({
                "id":"hk","name":"Survey DEM","version":"0123456789abcdef",
                "bounds":[113.8,22.1,114.5,22.6],"minzoom":8,"maxzoom":15,
                "encoding":"terrarium","tile_size":256,"vertical_datum":"HKPD",
                "resolution_m":5,"attribution":"LandsD","source_sha256":"survey-hash",
                "tiles":["https://untrusted.invalid/tile"]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("hk/0123456789abcdef/tiles/8/209/111.png"),
            b"\x89PNG\r\n\x1a\nfixture",
        )
        .unwrap();
        Fixture(root)
    }

    #[test]
    fn discovery_retains_survey_metadata_and_generates_local_urls() {
        let f = fixture();
        let result = discover(&f.0).unwrap();
        assert_eq!(
            result["sources"][0]["tiles"][0],
            "/v1/terrain/hk/0123456789abcdef/{z}/{x}/{y}.png"
        );
        assert_eq!(result["sources"][0]["vertical_datum"], "HKPD");
        assert_eq!(result["sources"][0]["source_sha256"], "survey-hash");
        fs::create_dir(f.0.join("broken")).unwrap();
        fs::write(f.0.join("broken/manifest.json"), b"{}").unwrap();
        let result = discover(&f.0).unwrap();
        assert_eq!(result["sources"].as_array().unwrap().len(), 1);
        assert_eq!(result["diagnostics"].as_array().unwrap().len(), 1);
        assert_eq!(
            discover(&f.0.join("not-prepared")).unwrap(),
            json!({"sources":[]})
        );
    }

    #[test]
    fn tiles_are_version_cached_and_missing_tiles_are_not_fabricated() {
        let f = fixture();
        let response = tile_response(
            &f.0,
            "hk",
            "0123456789abcdef",
            8,
            209,
            "111.png",
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert!(
            response.headers()[header::CACHE_CONTROL]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            response.headers()[header::ETAG].clone(),
        );
        assert_eq!(
            tile_response(&f.0, "hk", "0123456789abcdef", 8, 209, "111.png", &headers)
                .unwrap()
                .status(),
            StatusCode::NOT_MODIFIED
        );
        assert_eq!(
            tile_response(&f.0, "hk", "0123456789abcdef", 8, 209, "112.png", &headers)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn rejects_traversal_and_out_of_grid_coordinates() {
        let f = fixture();
        for (id, version, z, x, tile) in [
            ("..", "0123456789abcdef", 8, 209, "111.png"),
            ("hk", "../0123456789abcdef", 8, 209, "111.png"),
            ("hk", "0123456789abcdef", 25, 209, "111.png"),
            ("hk", "0123456789abcdef", 8, 256, "111.png"),
            ("hk", "0123456789abcdef", 8, 209, "256.png"),
            ("hk", "0123456789abcdef", 8, 209, "../111.png"),
        ] {
            assert_eq!(
                tile_response(&f.0, id, version, z, x, tile, &HeaderMap::new())
                    .unwrap_err()
                    .into_response()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_tiles_symlinked_outside_terrain_directory() {
        let f = fixture();
        std::os::unix::fs::symlink(
            "/etc/hosts",
            f.0.join("hk/0123456789abcdef/tiles/8/209/112.png"),
        )
        .unwrap();
        assert_eq!(
            tile_response(
                &f.0,
                "hk",
                "0123456789abcdef",
                8,
                209,
                "112.png",
                &HeaderMap::new()
            )
            .unwrap_err()
            .into_response()
            .status(),
            StatusCode::NOT_FOUND
        );
    }
}
