use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use docparse_server::{
    app::router,
    state::{AppState, HttpOptions},
    storage::SharedStorage,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Metadata routes must expose every API operation and a working Scalar document independently of database availability.
#[tokio::test]
async fn documentation_covers_routes_and_wire_schemas() {
    let directory = tempfile::tempdir().expect("storage");
    let state = AppState::new(
        Default::default(),
        SharedStorage::new(directory.path()).await.expect("storage"),
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("state");
    let app = router(state, &docparse_config::ServerConfig::default())
        .expect("router");
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/openapi.json")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .expect("spec body");
    let spec: Value = serde_json::from_slice(&bytes).expect("OpenAPI JSON");
    // Include monitoring in both operation coverage and the expected path count.
    let routes = [
        ("/api/jobs", "post"),
        ("/api/jobs/status", "get"),
        ("/api/jobs/events", "get"),
        ("/api/jobs/result", "get"),
        ("/api/jobs/list", "get"),
        ("/api/jobs/source", "get"),
        ("/api/jobs/delete", "post"),
        ("/api/health", "get"),
        ("/api/ready", "get"),
        ("/api/openapi.json", "get"),
        ("/api/docs", "get"),
        ("/api/monitoring/snapshot", "get"),
        ("/api/monitoring/history", "get"),
    ];
    for (path, method) in routes {
        assert!(
            spec.get("paths")
                .and_then(|paths| paths.get(path))
                .and_then(|path| path.get(method))
                .is_some(),
            "missing {method} {path}"
        );
    }
    check_references(&spec, &spec);
    let page = spec
        .pointer("/paths/~1api~1jobs~1result/get/parameters")
        .and_then(Value::as_array)
        .expect("result parameters")
        .iter()
        .find(|parameter| {
            parameter.get("name").and_then(Value::as_str) == Some("page")
        })
        .expect("page parameter");
    assert_eq!(page.get("required"), Some(&Value::Bool(false)));
    assert_eq!(
        page.pointer("/schema/minimum").and_then(Value::as_f64),
        Some(1.0)
    );
    assert!(
        spec.pointer("/paths/~1api~1jobs~1result/get/parameters")
            .and_then(Value::as_array)
            .expect("result parameters")
            .iter()
            .any(|parameter| parameter.get("name").and_then(Value::as_str)
                == Some("format")),
        "result API must support JSON/Markdown selection"
    );
    assert!(
        spec.pointer("/components/schemas/FormulaResult/properties/latex")
            .is_some()
    );
    assert!(
        spec.pointer("/components/schemas/FormulaResult/properties/markdown")
            .is_some()
    );
    assert!(
        spec.pointer("/components/schemas/Block/properties/markdown")
            .is_some()
    );
    assert!(
        spec.pointer("/components/schemas/FormulaResult/properties/crop_bbox")
            .is_some()
    );
    // All job identifiers are required query parameters; the deployed spec must not expose capture paths.
    // The PDF source shares the same durable identifier and prefix as status and result queries.
    for (endpoint, method) in [
        ("status", "get"),
        ("events", "get"),
        ("result", "get"),
        ("source", "get"),
        ("delete", "post"),
    ] {
        let operation = spec
            .pointer(&format!("/paths/~1api~1jobs~1{endpoint}/{method}"))
            .expect("job operation");
        assert_eq!(operation.get("tags"), Some(&serde_json::json!(["JOBS"])));
        let parameters = operation
            .get("parameters")
            .and_then(Value::as_array)
            .expect("query parameters");
        assert!(parameters.iter().any(|parameter| {
            parameter.get("name").and_then(Value::as_str) == Some("id")
                && parameter.get("in").and_then(Value::as_str) == Some("query")
                && parameter.get("required").and_then(Value::as_bool)
                    == Some(true)
                && parameter.pointer("/schema/format").and_then(Value::as_str)
                    == Some("uuid")
        }));
        assert!(
            parameters
                .iter()
                .all(|parameter| parameter.get("in").and_then(Value::as_str)
                    != Some("path"))
        );
    }
    assert_eq!(
        spec.pointer("/paths/~1api~1jobs/post/tags"),
        Some(&serde_json::json!(["JOBS"]))
    );
    assert_eq!(
        spec.get("paths")
            .and_then(Value::as_object)
            .expect("paths")
            .len(),
        routes.len()
    );
    let schemas = spec.pointer("/components/schemas").expect("schemas");
    assert!(
        schemas
            .pointer("/ApiErrorResponse/properties/http_code")
            .is_none(),
        "transport status is not part of the JSON envelope"
    );
    assert_eq!(
        schemas
            .pointer("/DocumentResult/properties/schema_version/type")
            .and_then(Value::as_str),
        Some("string")
    );
    assert_eq!(
        schemas.pointer("/Polygon/type").and_then(Value::as_str),
        Some("array")
    );
    assert_eq!(
        schemas
            .pointer("/Polygon/items/properties/x/type")
            .and_then(Value::as_str),
        Some("number")
    );
    assert_eq!(
        schemas
            .pointer("/PdfUpload/properties/file/format")
            .and_then(Value::as_str),
        Some("binary")
    );
    assert_eq!(
        schemas.pointer("/BlockId/type").and_then(Value::as_str),
        Some("string")
    );
    for bound in ["start", "end"] {
        assert_eq!(schemas.pointer(&format!("/TableTextSpan/properties/byte_range/properties/{bound}/type")).and_then(Value::as_str), Some("integer"));
    }
    assert_eq!(
        schemas.pointer("/JobStatus/enum"),
        Some(&serde_json::json!([
            "queued",
            "running",
            "succeeded",
            "failed"
        ]))
    );
    let params = spec
        .pointer("/paths/~1api~1jobs/post/parameters")
        .and_then(Value::as_array)
        .expect("upload parameters");
    assert!(params.iter().any(
        |param| param.get("name").and_then(Value::as_str)
            == Some("Idempotency-Key")
            && param.get("required").and_then(Value::as_bool) == Some(true)
    ));
    assert!(
        spec.pointer(
            "/paths/~1api~1jobs/post/requestBody/content/multipart~1form-data"
        )
        .is_some()
    );
    let frame = spec.pointer("/paths/~1api~1jobs~1events/get/responses/200/content/text~1event-stream/example").and_then(Value::as_str).expect("SSE example");
    assert!(frame.starts_with("event: job\nid: 4\n"));
    assert!(frame.ends_with("\n\n"));
    let payload = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("SSE data");
    let payload: Value = serde_json::from_str(payload).expect("SSE JSON");
    assert_eq!(
        payload
            .pointer("/data/progress/stage")
            .and_then(Value::as_str),
        Some("complete")
    );
    for name in [
        "PageResult",
        "Block",
        "Line",
        "TextItem",
        "Table",
        "TableCell",
        "ApiErrorResponse",
        "ParseProgress",
    ] {
        assert!(schemas.get(name).is_some(), "missing nested schema {name}");
    }
    let response = app
        .oneshot(
            Request::get("/api/docs")
                .body(Body::empty())
                .expect("docs request"),
        )
        .await
        .expect("router");
    assert_eq!(response.status(), StatusCode::OK);
    let html = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .expect("HTML");
    let html = String::from_utf8_lossy(&html);
    assert!(html.contains("@scalar/api-reference"));
    assert!(html.contains("/api/jobs/status"));
}

/// Root and custom prefixes must agree across live handlers, OpenAPI paths and Scalar's embedded document.
#[tokio::test]
async fn custom_and_root_prefixes_match_live_routes_and_documentation() {
    let directory = tempfile::tempdir().expect("storage");
    let state = AppState::new(
        Default::default(),
        SharedStorage::new(directory.path()).await.expect("storage"),
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("state");
    for prefix in ["", "/", "/service/docparse/v2"] {
        let config = docparse_config::ServerConfig::builder()
            .api_prefix(prefix)
            .build();
        let app = router(state.clone(), &config).expect("configured router");
        let base = prefix.trim_end_matches('/');
        for route in ["health", "docs", "openapi.json"] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("{base}/{route}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{prefix}: {route}");
            let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .expect("body");
            if route == "openapi.json" {
                let spec: Value =
                    serde_json::from_slice(&body).expect("document");
                let paths = spec
                    .get("paths")
                    .and_then(Value::as_object)
                    .expect("paths");
                for path in paths.keys() {
                    assert!(path.starts_with(&format!("{base}/")));
                    assert!(
                        !path.contains('{'),
                        "path captures are no longer exposed"
                    );
                }
                assert!(paths.contains_key(&format!("{base}/jobs/status")));
            } else if route == "docs" {
                assert!(
                    String::from_utf8_lossy(&body)
                        .contains(&format!("{base}/jobs/status"))
                );
            }
        }
        let rejected = app
            .oneshot(
                Request::get("/api/health")
                    .body(Body::empty())
                    .expect("old prefix"),
            )
            .await
            .expect("response");
        assert_eq!(rejected.status(), StatusCode::NOT_FOUND);
    }
    // Validation reports bad configuration before Axum can panic on route syntax.
    let config = docparse_config::ServerConfig::builder()
        .api_prefix("/{tenant}")
        .build();
    assert!(matches!(
        router(state, &config),
        Err(docparse_config::ConfigError::InvalidValue {
            field: "server.api_prefix",
            ..
        })
    ));
}

/// Generated component references must all resolve, including deeply nested document geometry and table data.
fn check_references(value: &Value, spec: &Value) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix('#'))
            {
                assert!(
                    spec.pointer(reference).is_some(),
                    "unresolved {reference}"
                );
            }
            for child in object.values() {
                check_references(child, spec);
            }
        }
        Value::Array(array) => {
            for child in array {
                check_references(child, spec);
            }
        }
        _ => {}
    }
}
