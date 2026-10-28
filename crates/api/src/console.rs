//! Serves the web console: from a directory when one is given, otherwise from
//! the copy embedded at build time (see `build.rs`). Unknown paths fall back
//! to `index.html` so the single-page console handles them.

use axum::Router;
use axum::body::Body;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::path::Path;
use tower_http::services::{ServeDir, ServeFile};

include!(concat!(env!("OUT_DIR"), "/console_assets.rs"));

/// Whether a console was compiled into this binary.
pub fn embedded_console_available() -> bool {
    CONSOLE_ASSETS.iter().any(|(name, _)| *name == "index.html")
}

pub(crate) fn attach_console(
    router: Router<crate::state::ApiState>,
    dir: Option<&Path>,
) -> Router<crate::state::ApiState> {
    if let Some(dir) = dir {
        let index = dir.join("index.html");
        return router
            .fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)));
    }
    if embedded_console_available() {
        return router
            .route("/", get(|| async { embedded("index.html") }))
            .fallback(get(|uri: Uri| async move { embedded(uri.path()) }));
    }
    router
}

fn embedded(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    let lookup = if path.is_empty() { "index.html" } else { path };
    let found = CONSOLE_ASSETS
        .iter()
        .find(|(name, _)| *name == lookup)
        .or_else(|| {
            // API paths are never served by the console fallback.
            if lookup.starts_with("v1/") || lookup == "healthz" || lookup == "readyz" {
                None
            } else {
                CONSOLE_ASSETS
                    .iter()
                    .find(|(name, _)| *name == "index.html")
            }
        });
    match found {
        Some((name, bytes)) => {
            let mime = mime_guess::from_path(name).first_or_octet_stream();
            let cache = if name.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                Body::from(*bytes),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
