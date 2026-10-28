use std::collections::BTreeSet;

use axum::http::header;
use serde_yaml::Value;

use super::*;

const LIB_SOURCE: &str = include_str!("../lib.rs");
const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

fn spec() -> Value {
    serde_yaml::from_str(OPENAPI_YAML).expect("docs/api/openapi.yaml parses as YAML")
}

/// `METHOD path` for every route registered in `router()` in `lib.rs`.
fn router_operations() -> BTreeSet<String> {
    let start = LIB_SOURCE
        .find("fn router(")
        .expect("lib.rs defines fn router");
    let body = &LIB_SOURCE[start..];
    let end = body
        .find("console::attach_console")
        .expect("router() attaches the console after its routes");
    let body = &body[..end];
    let mut operations = BTreeSet::new();
    for segment in body.split(".route(").skip(1) {
        let path = string_literal(segment).expect("every .route( starts with a path literal");
        let methods = route_methods(segment);
        assert!(!methods.is_empty(), "route {path} registers no method");
        for method in methods {
            operations.insert(format!("{} {}", method.to_uppercase(), normalize(&path)));
        }
    }
    operations
}

fn string_literal(segment: &str) -> Option<String> {
    let open = segment.find('"')?;
    let rest = &segment[open + 1..];
    let close = rest.find('"')?;
    Some(rest[..close].to_string())
}

/// Method routers (`get(`, `.post(`, `axum::routing::delete(`) in one
/// `.route(...)` call; handler names such as `get_job(` do not match.
fn route_methods(segment: &str) -> Vec<&'static str> {
    let bytes = segment.as_bytes();
    let mut methods = Vec::new();
    for method in METHODS {
        let needle = format!("{method}(");
        let mut from = 0;
        while let Some(found) = segment[from..].find(&needle) {
            let at = from + found;
            let preceded_by_ident = at > 0 && {
                let previous = bytes[at - 1];
                previous.is_ascii_alphanumeric() || previous == b'_'
            };
            if !preceded_by_ident {
                methods.push(method);
            }
            from = at + needle.len();
        }
    }
    methods
}

/// Axum 0.7 `:param` and wildcard `{*rest}` segments become `{param}`.
fn normalize(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if let Some(name) = segment.strip_prefix(':') {
                format!("{{{name}}}")
            } else if let Some(name) = segment.strip_prefix("{*") {
                format!("{{{name}")
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn spec_operations(spec: &Value) -> BTreeSet<String> {
    let paths = spec["paths"].as_mapping().expect("spec has paths");
    let mut operations = BTreeSet::new();
    for (path, item) in paths {
        let path = path.as_str().expect("path keys are strings");
        let item = item.as_mapping().expect("path items are mappings");
        for (key, _) in item {
            let key = key.as_str().expect("path item keys are strings");
            if METHODS.contains(&key) {
                operations.insert(format!("{} {}", key.to_uppercase(), normalize(path)));
            }
        }
    }
    operations
}

#[test]
fn router_parser_finds_known_routes() {
    let operations = router_operations();
    for expected in [
        "GET /healthz",
        "POST /v1/route",
        "POST /v1/traffic/live",
        "DELETE /v1/traffic/live",
        "DELETE /v1/workspace/uploads/{name}",
        "PUT /v1/gtfs-editor/scenarios/{scenario_id}",
        "GET /openapi.yaml",
        "GET /docs",
    ] {
        assert!(
            operations.contains(expected),
            "router parser missed {expected}"
        );
    }
    assert!(!operations.contains("GET /v1/workspace/jobs/{job_id}/get_job"));
}

#[test]
fn spec_documents_every_router_operation_and_nothing_else() {
    let router = router_operations();
    let spec = spec_operations(&spec());
    let undocumented: Vec<_> = router.difference(&spec).collect();
    let unrouted: Vec<_> = spec.difference(&router).collect();
    assert!(
        undocumented.is_empty() && unrouted.is_empty(),
        "docs/api/openapi.yaml and router() disagree.\n\
         Routed but not documented: {undocumented:#?}\n\
         Documented but not routed: {unrouted:#?}"
    );
}

#[test]
fn spec_operations_are_complete_and_references_resolve() {
    let spec = spec();
    assert_eq!(spec["openapi"].as_str(), Some("3.1.0"));
    let tags: BTreeSet<_> = spec["tags"]
        .as_sequence()
        .expect("spec declares tags")
        .iter()
        .map(|tag| tag["name"].as_str().expect("tag name").to_string())
        .collect();
    let mut operation_ids = BTreeSet::new();
    for (path, item) in spec["paths"].as_mapping().expect("paths") {
        for (method, operation) in item.as_mapping().expect("path item") {
            let label = format!("{} {}", method.as_str().unwrap(), path.as_str().unwrap());
            for field in ["summary", "description", "operationId"] {
                assert!(
                    operation[field]
                        .as_str()
                        .is_some_and(|text| !text.is_empty()),
                    "{label} has no {field}"
                );
            }
            assert!(
                operation_ids.insert(operation["operationId"].as_str().unwrap().to_string()),
                "{label} repeats an operationId"
            );
            let operation_tags = operation["tags"].as_sequence().expect("operation tags");
            assert!(!operation_tags.is_empty(), "{label} has no tag");
            for tag in operation_tags {
                assert!(
                    tags.contains(tag.as_str().unwrap()),
                    "{label} uses undeclared tag {tag:?}"
                );
            }
            assert!(
                operation["responses"]
                    .as_mapping()
                    .is_some_and(|responses| !responses.is_empty()),
                "{label} has no responses"
            );
        }
    }
    let mut references = Vec::new();
    collect_references(&spec, &mut references);
    assert!(!references.is_empty());
    for reference in references {
        let pointer = reference
            .strip_prefix("#/")
            .unwrap_or_else(|| panic!("{reference} is not a local reference"));
        let mut target = &spec;
        for part in pointer.split('/') {
            target = target
                .get(part)
                .unwrap_or_else(|| panic!("{reference} does not resolve"));
        }
    }
}

fn collect_references<'a>(value: &'a Value, references: &mut Vec<&'a str>) {
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                if key.as_str() == Some("$ref") {
                    references.push(child.as_str().expect("$ref is a string"));
                } else {
                    collect_references(child, references);
                }
            }
        }
        Value::Sequence(sequence) => {
            for child in sequence {
                collect_references(child, references);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn serves_the_spec_as_yaml() {
    let response = openapi_yaml().await;
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/yaml");
}

#[test]
fn docs_page_loads_the_spec_with_pinned_swagger_ui() {
    assert!(SWAGGER_UI_HTML.contains(r#"url: "openapi.yaml""#));
    assert_eq!(
        SWAGGER_UI_HTML.matches("swagger-ui-dist@5.34.0/").count(),
        2
    );
    assert_eq!(SWAGGER_UI_HTML.matches("integrity=\"sha384-").count(), 2);
}
