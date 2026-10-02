use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const VERSION: &str = "0123456789abcdef";

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Prepared terrain directory with one `hk` source and a single z8 tile.
struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "netweevil-terrain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("hk/0123456789abcdef/tiles/8/209")).unwrap();
        fs::write(
            root.join("hk/manifest.json"),
            serde_json::to_vec(&json!({
                "id":"hk","name":"Survey DEM","version":VERSION,
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
        Self(root)
    }

    fn tile_status(&self, id: &str, version: &str, z: u8, x: u32, tile: &str) -> StatusCode {
        match tile_response(&self.0, id, version, z, x, tile, &HeaderMap::new()) {
            Ok(response) => response.status(),
            Err(error) => error.into_response().status(),
        }
    }
}

#[test]
fn discovery_retains_survey_metadata_and_generates_local_urls() {
    let f = Fixture::new();
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
    let f = Fixture::new();
    let response =
        tile_response(&f.0, "hk", VERSION, 8, 209, "111.png", &HeaderMap::new()).unwrap();
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
        tile_response(&f.0, "hk", VERSION, 8, 209, "111.png", &headers)
            .unwrap()
            .status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        f.tile_status("hk", VERSION, 8, 209, "112.png"),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn rejects_traversal_and_out_of_grid_coordinates() {
    let f = Fixture::new();
    for (id, version, z, x, tile) in [
        ("..", VERSION, 8, 209, "111.png"),
        ("hk", "../0123456789abcdef", 8, 209, "111.png"),
        ("hk", VERSION, 25, 209, "111.png"),
        ("hk", VERSION, 8, 256, "111.png"),
        ("hk", VERSION, 8, 209, "256.png"),
        ("hk", VERSION, 8, 209, "../111.png"),
    ] {
        assert_eq!(
            f.tile_status(id, version, z, x, tile),
            StatusCode::BAD_REQUEST,
            "{id}/{version}/{z}/{x}/{tile}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_tiles_symlinked_outside_terrain_directory() {
    let f = Fixture::new();
    std::os::unix::fs::symlink(
        "/etc/hosts",
        f.0.join("hk/0123456789abcdef/tiles/8/209/112.png"),
    )
    .unwrap();
    assert_eq!(
        f.tile_status("hk", VERSION, 8, 209, "112.png"),
        StatusCode::NOT_FOUND
    );
}
