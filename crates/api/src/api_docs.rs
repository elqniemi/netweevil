//! The OpenAPI description of this API (`docs/api/openapi.yaml`, embedded at
//! compile time) and a Swagger UI page that renders it.

use axum::http::header;
use axum::response::{Html, IntoResponse, Response};

pub(crate) const OPENAPI_YAML: &str = include_str!("../../../docs/api/openapi.yaml");

/// Swagger UI from a pinned `swagger-ui-dist` release; the integrity hashes
/// pin the exact files.
const SWAGGER_UI_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>NetWeevil API reference</title>
  <link rel="stylesheet"
    href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.0/swagger-ui.css"
    integrity="sha384-4c3vsE5f+trhFyHrFrLM1fFGUn531NIaC7dcqEWIJdoRQJ5CkJbSuSCwXLpaS1KF"
    crossorigin="anonymous">
</head>
<body>
  <div id="swagger-ui"></div>
  <script
    src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.0/swagger-ui-bundle.js"
    integrity="sha384-9Td7MKiIr2S83E1kZyyVi8KdJw/S8CpAvu0llLyMeg0xq/hDGiZJfVn0fV4llCmK"
    crossorigin="anonymous"></script>
  <script>
    window.ui = SwaggerUIBundle({
      url: "openapi.yaml",
      dom_id: "#swagger-ui",
      deepLinking: true,
    });
  </script>
</body>
</html>
"##;

pub(crate) async fn openapi_yaml() -> Response {
    ([(header::CONTENT_TYPE, "application/yaml")], OPENAPI_YAML).into_response()
}

pub(crate) async fn swagger_ui() -> Html<&'static str> {
    Html(SWAGGER_UI_HTML)
}

#[cfg(test)]
mod tests;
