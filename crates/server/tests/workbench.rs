//! Workbench serving checks: the UI lives at `{api_prefix}/webui` while the API keeps its JSON envelope.
//!
//! The invariants under test: the mount serves the shell, `/` redirects to it, the bare
//! API prefix and every neighbouring shape stay JSON, a missing hashed asset stays an
//! uncached 404, and precompressed, conditional and range handling stay with tower-http
//! for both the directory and the embedded source.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::Response,
};
use docparse_config::{ConfigError, ServerConfig, WebUi};
use docparse_server::{
    app::router,
    state::{AppState, HttpOptions},
    storage::SharedStorage,
};
use flate2::{Compression, write::GzEncoder};
use std::{io::Write, path::Path};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// API prefix shared by the tests.
const PREFIX: &str = "/api/v1/docparse";
/// Mount path derived from the prefix.
const MOUNT: &str = "/api/v1/docparse/webui";

/// Shell marker used to prove that a response is the SPA entry document.
const SHELL: &str =
    "<!doctype html><html><body><div id=\"root\"></div></body></html>";

/// Stable embedded file used to prove the compiled-in backend serves assets.
///
/// It comes from the pinned PDF.js asset set, so a PDF.js upgrade that renames
/// it must update this probe.
#[cfg(feature = "embed-web")]
const EMBEDDED_ASSET: &str = "pdfjs/wasm/jbig2.wasm";

/// Writes one gzip sibling the way `packages/web/scripts/compress-assets.mjs` does.
fn write_precompressed(path: &Path, contents: &[u8]) {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(contents).expect("gzip write");
    std::fs::write(path, encoder.finish().expect("gzip finish"))
        .expect("compressed asset");
}

/// Builds a workbench directory with an entry document, one hashed bundle and its gzip sibling.
fn workbench_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("workbench directory");
    std::fs::write(directory.path().join("index.html"), SHELL)
        .expect("entry document");
    let assets = directory.path().join("assets");
    std::fs::create_dir(&assets).expect("assets directory");
    let bundle = b"console.log(\"hashed bundle\");";
    std::fs::write(assets.join("index-DEADBEEF.js"), bundle).expect("bundle");
    write_precompressed(&assets.join("index-DEADBEEF.js.gz"), bundle);
    directory
}

/// Creates a router with its storage directory, which must outlive the router.
async fn application(config: &ServerConfig) -> (Router, tempfile::TempDir) {
    let storage = tempfile::tempdir().expect("storage");
    let state = AppState::new(
        Default::default(),
        SharedStorage::new(storage.path()).await.expect("storage"),
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("state");
    let app = router(state, config).expect("configured router");
    (app, storage)
}

/// Creates a router whose workbench directory is the given one.
async fn workbench_application(root: &Path) -> (Router, tempfile::TempDir) {
    application(
        &ServerConfig::builder()
            .api_prefix(PREFIX)
            .webui(WebUi::Disk(root.to_path_buf()))
            .build(),
    )
    .await
}

/// Sends one request and returns the raw response.
async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.expect("response")
}

/// Builds a `GET` request for one path.
fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
}

/// Reads a response body as text for content assertions.
async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Reports whether the response carries the routine marker read by the trace layer.
fn is_routine(response: &Response) -> bool {
    response
        .extensions()
        .get::<docparse_server::middlewares::RoutineCompletion>()
        .is_some()
}

/// Reads one response header as text.
fn header_text(response: &Response, name: header::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().expect("ascii header").to_owned())
        .unwrap_or_default()
}

/// Client-side routes and the mount root resolve to the shell.
#[tokio::test]
async fn client_side_routes_serve_the_shell() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    for path in [
        MOUNT.to_owned(),
        format!("{MOUNT}/"),
        format!("{MOUNT}/document"),
        format!("{MOUNT}/monitoring"),
    ] {
        let response = send(&app, get(&path)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            header_text(&response, header::CONTENT_TYPE)
                .starts_with("text/html"),
            "{path}"
        );
        assert_eq!(
            header_text(&response, header::CACHE_CONTROL),
            "no-cache",
            "{path}"
        );
        assert!(body_text(response).await.contains("id=\"root\""), "{path}");
    }
}

/// The root redirects to the mount so the UI stays discoverable.
#[tokio::test]
async fn root_redirects_to_the_mount() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let response = send(&app, get("/")).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_text(&response, header::LOCATION),
        format!("{MOUNT}/")
    );

    // A bookmarked deep link keeps its query through the hop.
    let response = send(&app, get("/?job=42&page=3")).await;
    assert_eq!(
        header_text(&response, header::LOCATION),
        format!("{MOUNT}/?job=42&page=3")
    );
}

/// The entry document is revalidated instead of being cached forever.
#[tokio::test]
async fn entry_document_is_revalidated_not_cached() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let route = format!("{MOUNT}/document");
    let response = send(&app, get(&route)).await;
    let etag = header_text(&response, header::ETAG);
    assert!(!etag.is_empty(), "the entry document needs a validator");

    let response = send(
        &app,
        Request::get(&route)
            .header(header::IF_NONE_MATCH, &etag)
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(header_text(&response, header::CACHE_CONTROL), "no-cache");
}

/// HEAD requests return the entry document's validators without a body.
#[tokio::test]
async fn head_requests_return_validators_without_a_body() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let response = send(
        &app,
        Request::head(format!("{MOUNT}/document"))
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(header_text(&response, header::ETAG).starts_with('"'));
    assert!(body_text(response).await.is_empty());
}

/// Content-hashed bundles are immutable and negotiate their gzip sibling.
#[tokio::test]
async fn hashed_assets_are_immutable_and_precompressed() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let bundle = format!("{MOUNT}/assets/index-DEADBEEF.js");

    let response = send(
        &app,
        Request::get(&bundle)
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_text(&response, header::CONTENT_ENCODING), "gzip");
    assert_eq!(
        header_text(&response, header::VARY).to_ascii_lowercase(),
        "accept-encoding"
    );
    assert_eq!(
        header_text(&response, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );

    // Without gzip support the plain file is served with the same policy.
    let response = send(&app, get(&bundle)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(header_text(&response, header::CONTENT_ENCODING).is_empty());
    assert_eq!(
        header_text(&response, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
}

/// Revalidating a hashed asset keeps its immutable policy.
#[tokio::test]
async fn revalidated_hashed_asset_keeps_its_cache_policy() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let bundle = format!("{MOUNT}/assets/index-DEADBEEF.js");
    let response = send(&app, get(&bundle)).await;
    let etag = header_text(&response, header::ETAG);
    assert!(!etag.is_empty(), "the bundle needs a validator");

    let response = send(
        &app,
        Request::get(&bundle)
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_text(&response, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
}

/// Ranges stay byte-accurate, and encoded variants are ranged consistently.
///
/// Without gzip support the range applies to the plain file. With gzip support
/// tower-http ranges the selected gzip representation and keeps its
/// `Content-Encoding`, which is what RFC 9110 range semantics describe; the
/// upload source endpoint, where browsers do resume PDFs, serves no
/// precompressed variants.
#[tokio::test]
async fn asset_ranges_report_the_requested_window() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let bundle = format!("{MOUNT}/assets/index-DEADBEEF.js");

    let response = send(
        &app,
        Request::get(&bundle)
            .header(header::RANGE, "bytes=0-6")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header_text(&response, header::CONTENT_RANGE),
        "bytes 0-6/29"
    );
    assert!(
        header_text(&response, header::CONTENT_ENCODING).is_empty(),
        "the plain representation is not encoded"
    );
    assert_eq!(body_text(response).await, "console");

    let response = send(
        &app,
        Request::get(&bundle)
            .header(header::RANGE, "bytes=0-6")
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_text(&response, header::CONTENT_ENCODING), "gzip");
    assert!(
        header_text(&response, header::CONTENT_RANGE).starts_with("bytes 0-6/"),
        "the range describes the encoded representation"
    );
}

/// A missing hashed asset stays a real, uncached 404 instead of returning the shell.
#[tokio::test]
async fn missing_hashed_asset_is_an_uncached_404() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    for path in [
        format!("{MOUNT}/assets/index-00000000.js"),
        format!("{MOUNT}/assets/"),
        format!("{MOUNT}/favicon.ico"),
    ] {
        let response = send(&app, get(&path)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            header_text(&response, header::CACHE_CONTROL),
            "no-cache",
            "{path}"
        );
        assert!(!body_text(response).await.contains("id=\"root\""), "{path}");
    }
}

/// A directory inside the mount redirects with the mount preserved in the location.
#[tokio::test]
async fn directory_without_trailing_slash_redirects() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let response = send(&app, get(&format!("{MOUNT}/assets"))).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_text(&response, header::LOCATION),
        format!("{MOUNT}/assets/")
    );
}

/// The routine marker follows the status, not the whole router.
///
/// Served and revalidated workbench responses are routine traffic; a missing asset
/// and an API response are not, so failures keep their `INFO` line.
#[tokio::test]
async fn routine_marking_follows_the_response_status() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;

    let entry = format!("{MOUNT}/document");
    let response = send(&app, get(&entry)).await;
    let etag = header_text(&response, header::ETAG);
    assert!(is_routine(&response), "{entry} must be routine");

    let revalidated = send(
        &app,
        Request::get(&entry)
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
    assert!(is_routine(&revalidated), "a revalidation is routine");

    let bundle = format!("{MOUNT}/assets/index-DEADBEEF.js");
    assert!(is_routine(&send(&app, get(&bundle)).await), "{bundle}");

    for path in [
        format!("{MOUNT}/assets/index-00000000.js"),
        format!("{PREFIX}/unknown"),
        "/metrics".to_owned(),
    ] {
        assert!(
            !is_routine(&send(&app, get(&path)).await),
            "{path} must stay visible to the trace layer"
        );
    }
}

/// The entry document resolves through the file service, so it keeps full HTTP semantics.
#[tokio::test]
async fn entry_document_is_served_through_the_file_service() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let route = format!("{MOUNT}/document");

    let response = send(&app, get(&route)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let length = body_text(response).await.len();
    assert_eq!(length, SHELL.len());

    let response = send(
        &app,
        Request::head(&route).body(Body::empty()).expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, header::CONTENT_LENGTH),
        length.to_string(),
        "HEAD reports the length the file service would send"
    );

    let response = send(
        &app,
        Request::get(&route)
            .header(header::RANGE, "bytes=0-9")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header_text(&response, header::CONTENT_RANGE),
        format!("bytes 0-9/{length}")
    );
}

/// An entry document that disappears after startup is a deployment fault, not a 404.
#[tokio::test]
async fn entry_document_vanishes_at_runtime() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    std::fs::remove_file(directory.path().join("index.html"))
        .expect("remove the entry document");

    let response = send(&app, get(&format!("{MOUNT}/document"))).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(header_text(&response, header::CACHE_CONTROL), "no-cache");

    // Assets keep working, so the failure is visible without hiding a broken deploy.
    let response =
        send(&app, get(&format!("{MOUNT}/assets/index-DEADBEEF.js"))).await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// An empty API prefix mounts the workbench at `/webui` and leaves the API at the root.
#[tokio::test]
async fn empty_api_prefix_mounts_the_workbench_at_webui() {
    let directory = workbench_directory();
    let (app, _storage) = application(
        &ServerConfig::builder()
            .api_prefix("")
            .webui(WebUi::Disk(directory.path().to_path_buf()))
            .build(),
    )
    .await;

    let response = send(&app, get("/")).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_text(&response, header::LOCATION), "/webui/");

    let response = send(&app, get("/webui/")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("id=\"root\""));

    let response = send(&app, get("/health")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        header_text(&response, header::CONTENT_TYPE)
            .starts_with("application/json")
    );

    // The root namespace belongs to the API, so `/document` is not the shell.
    let response = send(&app, get("/document")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Methods without a workbench representation report the method.
#[tokio::test]
async fn unsupported_methods_report_405() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let response = send(
        &app,
        Request::post(format!("{MOUNT}/document"))
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(header_text(&response, header::ALLOW), "GET,HEAD");
}

/// Every API shape keeps the typed JSON envelope, including paths beside the mount.
#[tokio::test]
async fn api_paths_keep_the_json_envelope() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    for path in [
        PREFIX.to_owned(),
        format!("{PREFIX}/"),
        format!("{PREFIX}/unknown"),
        // The mount is a child of the prefix: only the exact mount may serve the shell.
        format!("{MOUNT}x"),
        format!("{MOUNT}%2Fdocument"),
        "/webui".to_owned(),
        "/document".to_owned(),
    ] {
        let response = send(&app, get(&path)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(
            header_text(&response, header::CONTENT_TYPE)
                .starts_with("application/json"),
            "{path}"
        );
        assert!(
            body_text(response).await.contains("\"success\":false"),
            "{path}"
        );
    }
}

/// Root-level operational paths are not part of the SPA.
///
/// This harness installs no recorder, so the documented unavailable response is
/// the expected one; what matters is that the shell does not answer.
#[tokio::test]
async fn root_operational_paths_are_not_the_shell() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    let response = send(&app, get("/metrics")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body_text(response).await.contains("id=\"root\""));
}

/// Publishing a new shell takes effect without restarting the server.
#[tokio::test]
async fn replaced_entry_document_is_served_without_restart() {
    let directory = workbench_directory();
    let (app, _storage) = workbench_application(directory.path()).await;
    for path in [format!("{MOUNT}/"), format!("{MOUNT}/document")] {
        let response = send(&app, get(&path)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(body_text(response).await.contains("id=\"root\""), "{path}");
    }

    std::fs::write(
        directory.path().join("index.html"),
        SHELL.replace("id=\"root\"", "id=\"root\" data-build=\"next\""),
    )
    .expect("replaced entry document");

    for path in [format!("{MOUNT}/"), format!("{MOUNT}/document")] {
        let response = send(&app, get(&path)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            body_text(response).await.contains("data-build=\"next\""),
            "{path} still serves the previous deploy"
        );
    }
}

/// Without a configured workbench every path, including the root and the mount, stays JSON.
#[tokio::test]
async fn missing_workbench_configuration_keeps_json_fallbacks() {
    let (app, _storage) =
        application(&ServerConfig::builder().api_prefix(PREFIX).build()).await;
    for path in [
        "/".to_owned(),
        format!("{MOUNT}/"),
        format!("{MOUNT}/assets/index-DEADBEEF.js"),
    ] {
        let response = send(&app, get(&path)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(
            header_text(&response, header::CONTENT_TYPE)
                .starts_with("application/json"),
            "{path}"
        );
    }
}

/// A configured directory that cannot serve the application fails at startup.
#[tokio::test]
async fn workbench_rejects_an_unusable_directory() {
    let empty = tempfile::tempdir().expect("empty directory");
    for root in [empty.path().to_path_buf(), empty.path().join("absent")] {
        let storage = tempfile::tempdir().expect("storage");
        let state = AppState::new(
            Default::default(),
            SharedStorage::new(storage.path()).await.expect("storage"),
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state");
        let config = ServerConfig::builder()
            .api_prefix(PREFIX)
            .webui(WebUi::Disk(root))
            .build();
        let error = router(state, &config).expect_err("unusable directory");
        assert!(
            matches!(
                error,
                ConfigError::InvalidValue {
                    field: "server.webui",
                    ..
                }
            ),
            "{error}"
        );
    }
}

/// Without the `embed-web` feature the embedded source is rejected instead of ignored.
#[cfg(not(feature = "embed-web"))]
#[tokio::test]
async fn embedded_workbench_requires_the_feature() {
    let storage = tempfile::tempdir().expect("storage");
    let state = AppState::new(
        Default::default(),
        SharedStorage::new(storage.path()).await.expect("storage"),
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("state");
    let config = ServerConfig::builder()
        .api_prefix(PREFIX)
        .webui(WebUi::Embedded)
        .build();
    let error = router(state, &config).expect_err("feature is not enabled");
    assert!(
        matches!(
            error,
            ConfigError::InvalidValue {
                field: "server.webui",
                ..
            }
        ),
        "{error}"
    );
}

/// The embedded source serves the compiled build, or reports that none was embedded.
#[cfg(feature = "embed-web")]
#[tokio::test]
async fn embedded_workbench_serves_the_compiled_build() {
    let storage = tempfile::tempdir().expect("storage");
    let state = AppState::new(
        Default::default(),
        SharedStorage::new(storage.path()).await.expect("storage"),
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("state");
    let config = ServerConfig::builder()
        .api_prefix(PREFIX)
        .webui(WebUi::Embedded)
        .build();
    let app = match router(state, &config) {
        Ok(app) => app,
        Err(error) => {
            // A checkout without `packages/web/dist` must fail loudly. When a build
            // exists, rejecting it would be a defect rather than an accepted outcome.
            let built = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/web/dist/index.html");
            assert!(
                !built.is_file(),
                "a workbench build exists but embedding rejected it: {error}"
            );
            assert!(
                matches!(
                    error,
                    ConfigError::InvalidValue {
                        field: "server.webui",
                        ..
                    }
                ),
                "{error}"
            );
            return;
        }
    };

    let response = send(&app, get(&format!("{MOUNT}/document"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        header_text(&response, header::CONTENT_TYPE).starts_with("text/html")
    );
    assert_eq!(header_text(&response, header::CACHE_CONTROL), "no-cache");
    assert!(body_text(response).await.contains("id=\"root\""));

    let response = send(&app, get(MOUNT)).await;
    assert_eq!(response.status(), StatusCode::OK);

    // A stable, non-hashed file proves the backend resolves request paths rather
    // than only the entry document read at startup.
    let response = send(
        &app,
        Request::get(format!("{MOUNT}/{EMBEDDED_ASSET}"))
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{EMBEDDED_ASSET}");
    assert_eq!(
        header_text(&response, header::CONTENT_TYPE),
        "application/wasm",
        "{EMBEDDED_ASSET}"
    );
    assert_eq!(
        header_text(&response, header::CONTENT_ENCODING),
        "gzip",
        "{EMBEDDED_ASSET}"
    );

    // The same document resolves through the file service, so conditional requests,
    // ranges and HEAD match the directory source instead of a second implementation.
    let entry = format!("{MOUNT}/document");
    let response = send(&app, get(&entry)).await;
    let etag = header_text(&response, header::ETAG);
    assert!(
        !etag.is_empty(),
        "the embedded entry document needs a validator"
    );
    let response = send(
        &app,
        Request::get(&entry)
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let response = send(
        &app,
        Request::get(&entry)
            .header(header::RANGE, "bytes=0-9")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert!(
        header_text(&response, header::CONTENT_RANGE).starts_with("bytes 0-9/")
    );

    let response = send(
        &app,
        Request::head(&entry).body(Body::empty()).expect("request"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        header_text(&response, header::CONTENT_LENGTH)
            .parse::<usize>()
            .is_ok_and(|length| length > 0),
        "HEAD reports the entry document length"
    );

    // A missing embedded asset stays a 404 rather than the shell.
    let response =
        send(&app, get(&format!("{MOUNT}/assets/index-00000000.js"))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Directories exist only as key prefixes, yet they redirect exactly like disk.
    let response = send(&app, get(&format!("{MOUNT}/assets"))).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_text(&response, header::LOCATION),
        format!("{MOUNT}/assets/")
    );
}
