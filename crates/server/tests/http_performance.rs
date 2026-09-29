use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use docparse_server::{
    app::router,
    state::{AppState, HttpOptions},
    storage::SharedStorage,
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

// Saturation probes share the process CPU pool even when each test owns a separate runtime.
static CPU_SATURATION: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// Full HTTP bodies, including precompressed assets, must not wait for parser CPU admission.
#[test]
fn http_and_storage_remain_available_during_cpu_work() {
    use std::io::{Read, Write};
    use std::sync::{Arc, Condvar, Mutex};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let _saturation = CPU_SATURATION.lock().await;
        let directory = tempfile::tempdir().expect("storage");
        let web = directory.path().join("web");
        std::fs::create_dir_all(web.join("assets")).expect("web directory");
        std::fs::write(web.join("index.html"), "<html>CPU isolation</html>").expect("entry");
        let script = b"console.log('precompressed');".repeat(100);
        std::fs::write(web.join("assets/app-1234abcd.js"), &script).expect("script");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&script).expect("encode script");
        let compressed = encoder.finish().expect("gzip script");
        std::fs::write(web.join("assets/app-1234abcd.js.gz"), &compressed).expect("compressed script");
        let storage = SharedStorage::new(directory.path().join("data")).await.expect("storage");
        let app = router(
            AppState::new(Default::default(), storage.clone(), HttpOptions::builder().build(), CancellationToken::new()).expect("state"),
            &docparse_config::ServerConfig::builder().webui(docparse_config::WebUi::Disk(web)).build(),
        ).expect("router");
        // Match the documented parser capacity and wait until every slot is actually occupied.
        let capacity = std::thread::available_parallelism().map_or(1, usize::from).saturating_sub(1).max(1);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, mut entered) = tokio::sync::mpsc::unbounded_channel();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..capacity {
            let gate = Arc::clone(&gate);
            let started = started.clone();
            tasks.spawn(docparse_common::run_cpu(move || {
                started.send(()).expect("entered CPU slot");
                let released = gate.0.lock().expect("gate");
                let (released, _) = gate.1.wait_timeout_while(released, Duration::from_secs(10), |released| !*released).expect("release");
                assert!(*released, "test did not release the CPU gate");
            }));
        }
        for _ in 0..capacity { entered.recv().await.expect("occupied slot"); }
        let available = tokio::time::timeout(Duration::from_secs(2), async {
            let mut responses = Vec::new();
            for (path, encoding) in [
                ("/api/health", "identity"),
                ("/api/health", "gzip"),
                ("/api/webui/assets/app-1234abcd.js", "gzip"),
            ] {
                let response = app.clone().oneshot(Request::get(path).header("accept-encoding", encoding).body(Body::empty()).expect("request")).await.expect("response");
                let status = response.status();
                let encoding = response.headers().get("content-encoding").cloned();
                let body = to_bytes(response.into_body(), usize::MAX).await.expect("complete body");
                responses.push((status, encoding, body));
            }
            storage.ready().await.expect("filesystem capacity");
            responses
        }).await;
        // Release all native work before checking the result, even on the old failing implementation.
        *gate.0.lock().expect("gate") = true;
        gate.1.notify_all();
        while let Some(task) = tasks.join_next().await { task.expect("caller").expect("CPU work"); }
        let responses = available.expect("HTTP bodies and storage must remain available with a full CPU pool");
        for (status, _, _) in &responses { assert_eq!(*status, StatusCode::OK); }
        let (_, identity, plain) = responses.first().expect("identity health");
        assert!(identity.is_none());
        let (_, encoding, gzip) = responses.get(1).expect("gzip health");
        assert_eq!(encoding.as_ref().expect("gzip encoding"), "gzip");
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(gzip.as_ref()).read_to_end(&mut decoded).expect("valid gzip");
        assert_eq!(decoded, plain.as_ref());
        let (_, encoding, body) = responses.get(2).expect("precompressed asset");
        assert_eq!(encoding.as_ref().expect("gzip encoding"), "gzip");
        assert_eq!(body.as_ref(), compressed);
    });
}

/// Compression must be negotiated on the real router without altering the OpenAPI payload.
#[tokio::test]
async fn gzip_preserves_json_and_respects_identity() {
    let directory = tempfile::tempdir().expect("storage");
    let app = router(
        AppState::new(
            Default::default(),
            SharedStorage::new(directory.path()).await.expect("storage"),
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &Default::default(),
    )
    .expect("router");
    let plain = app
        .clone()
        .oneshot(
            Request::get("/api/openapi.json")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert!(!plain.headers().contains_key("content-encoding"));
    let expected = to_bytes(plain.into_body(), usize::MAX).await.expect("body");
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/openapi.json")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        response.headers().get("content-encoding").expect("gzip"),
        "gzip"
    );
    let encoded = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(encoded.as_ref()),
        &mut decoded,
    )
    .expect("valid gzip");
    assert_eq!(decoded, expected);
    let response = app
        .oneshot(
            Request::get("/api/openapi.json")
                .header("accept-encoding", "gzip;q=0, identity")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert!(!response.headers().contains_key("content-encoding"));
}

/// An invalid signature must fail before waiting for the rest of a stalled large upload.
#[tokio::test]
async fn invalid_pdf_is_rejected_before_upload_finishes() {
    use futures_util::{StreamExt, stream};
    let directory = tempfile::tempdir().expect("storage");
    let app = router(
        AppState::new(
            Default::default(),
            SharedStorage::new(directory.path()).await.expect("storage"),
            HttpOptions::builder()
                .upload_timeout(Duration::from_millis(100))
                .build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &Default::default(),
    )
    .expect("router");
    let chunks = stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"bad.pdf\"\r\n\r\nINVALID DATA THAT IS NOT A PDF"))]).chain(stream::pending());
    let response = app
        .oneshot(
            Request::post("/api/jobs")
                .header(
                    "content-type",
                    "multipart/form-data; boundary=boundary",
                )
                .header("idempotency-key", uuid::Uuid::new_v4().to_string())
                .body(Body::from_stream(chunks))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("storage")
            .count(),
        0
    );
}

/// Paging must preserve canonical bytes, revalidate cached representations, and clean derived files on deletion.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn result_pages_cache_and_cleanup() {
    let _saturation = CPU_SATURATION.lock().await;
    use docparse_core::{DocumentContext, DocumentResult, PageResult};
    use docparse_database::{
        connection, entities::parse_jobs::Entity,
        query::parse_job::ParseJobQuery as Jobs, seaorm::EntityTrait,
    };
    use docparse_server::model::base::ApiResponse;
    let db = connection::connect(
        &docparse_config::DatabaseConfig::builder()
            .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
            .build(),
    )
    .await
    .expect("database");
    let directory = tempfile::tempdir().expect("storage");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let id = uuid::Uuid::new_v4();
    Jobs::submit(&db, id, &"a".repeat(64), None, Some(8))
        .await
        .expect("submit");
    let lease = Jobs::claim(&db, 60, 3).await.expect("claim").expect("job");
    assert_eq!(lease.job.id, id);
    let pages = (1..=2)
        .map(|number| {
            PageResult::builder()
                .page_number(number)
                .width(100.0)
                .height(200.0)
                .rotation(0)
                .blocks(Vec::new())
                .diagnostics(std::collections::BTreeMap::from([(
                    "test".into(),
                    "中文 \\\" braces {} []".into(),
                )]))
                .build()
        })
        .collect();
    let document = DocumentResult::builder()
        .schema_version(serde_json::from_str("\"2.0\"").expect("version"))
        .context(DocumentContext::builder().page_count(2).build())
        .pages(pages)
        .build();
    let expected = serde_json::to_value(&document).expect("document");
    let mut temp = storage.temporary().await.expect("temp");
    // Pretty JSON exercises offsets independently of the compact worker serializer.
    serde_json::to_writer_pretty(
        temp.as_file_mut(),
        &ApiResponse::data(document),
    )
    .expect("JSON");
    storage.publish(temp, "result.json").await.expect("publish");
    Jobs::finish(&db, &lease, Ok("result.json"), None, Duration::from_secs(1))
        .await
        .expect("finish");
    let app = router(
        AppState::new(
            db.clone(),
            storage.clone(),
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &Default::default(),
    )
    .expect("router");
    // Warm the index before saturation: cache hits must not wait for parser admission either.
    storage
        .result_artifact("result.json", None)
        .await
        .expect("cached index");
    let capacity = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .saturating_sub(1)
        .max(1);
    let gate = std::sync::Arc::new((
        std::sync::Mutex::new(false),
        std::sync::Condvar::new(),
    ));
    let (started, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..capacity {
        let gate = std::sync::Arc::clone(&gate);
        let started = started.clone();
        tasks.spawn(docparse_common::run_cpu(move || {
            started.send(()).expect("CPU slot entered");
            let lock = gate.0.lock().expect("gate");
            let (released, _) = gate
                .1
                .wait_timeout_while(lock, Duration::from_secs(15), |released| {
                    !*released
                })
                .expect("gate release");
            assert!(*released, "test must release parser work");
        }));
    }
    for _ in 0..capacity {
        entered.recv().await.expect("occupied CPU slot");
    }
    let requests = async {
        for suffix in ["&page=2", "&page=1", ""] {
            let uri = format!("/api/jobs/result?id={id}{suffix}");
            let response = app
                .clone()
                .oneshot(
                    Request::get(&uri).body(Body::empty()).expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{suffix}");
            let etag = response.headers().get("etag").expect("etag").clone();
            let bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).expect("JSON");
            if suffix.is_empty() {
                assert_eq!(value.get("data"), Some(&expected));
            } else {
                let index = if suffix == "&page=2" { 1 } else { 0 };
                assert_eq!(
                    value.pointer("/data/page"),
                    expected.pointer(&format!("/pages/{index}"))
                );
                assert_eq!(
                    value
                        .pointer("/data/page_count")
                        .and_then(serde_json::Value::as_u64),
                    Some(2)
                );
            }
            let response = app
                .clone()
                .oneshot(
                    Request::get(uri)
                        .header(
                            "if-none-match",
                            format!(
                                "\"different\", {}",
                                etag.to_str().expect("etag")
                            ),
                        )
                        .header("accept-encoding", "gzip")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
            assert!(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body")
                    .is_empty()
            );
        }
        for suffix in ["&page=0", "&page=3", "&page=1&format=markdown"] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/jobs/result?id={id}{suffix}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{suffix}");
        }
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!(
                        "/api/jobs/result?id={id}&format=markdown"
                    ))
                    .body(Body::empty())
                    .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body")
                    .is_empty()
            );
        }
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("storage")
                .filter(|entry| entry
                    .as_ref()
                    .expect("entry")
                    .file_type()
                    .expect("file type")
                    .is_file())
                .count(),
            3,
            "source, index and cached Markdown"
        );
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/api/jobs/delete?id={id}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("storage")
                .filter(|entry| entry
                    .as_ref()
                    .expect("entry")
                    .file_type()
                    .expect("file type")
                    .is_file())
                .count(),
            0
        );
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/jobs/result?id={id}"))
                    .header("if-none-match", "*")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "conditional reads must check deletion first"
        );
        // Exercise final-only and batched hashing on the real upload route, then verify durable bytes.
        let pdf = include_bytes!(
            "../../core/tests/fixtures/pdf/extraction_metadata.pdf"
        );
        for padding in [0, 2 << 20] {
            let mut document = pdf.to_vec();
            document.resize(document.len() + padding, b' ');
            let expected_hash = blake3::hash(&document).to_hex().to_string();
            let upload_id = uuid::Uuid::new_v4();
            let mut multipart = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"sample.pdf\"\r\nContent-Type: application/pdf\r\n\r\n".to_vec();
            multipart.extend_from_slice(&document);
            multipart.extend_from_slice(b"\r\n--boundary--\r\n");
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/jobs")
                        .header(
                            "content-type",
                            "multipart/form-data; boundary=boundary",
                        )
                        .header("idempotency-key", upload_id.to_string())
                        .body(Body::from(multipart))
                        .expect("upload request"),
                )
                .await
                .expect("upload response");
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert_eq!(
                tokio::fs::read(
                    directory.path().join(format!("{expected_hash}.pdf"))
                )
                .await
                .expect("published input"),
                document
            );
            // Remove only this test's queued row; the public delete API deliberately rejects unfinished jobs.
            Entity::delete_by_id(upload_id)
                .exec(&db)
                .await
                .expect("remove test upload");
        }
    };
    let available =
        tokio::time::timeout(Duration::from_secs(5), requests).await;
    // Always release native work before reporting a timeout on the old implementation.
    *gate.0.lock().expect("gate") = true;
    gate.1.notify_all();
    while let Some(task) = tasks.join_next().await {
        task.expect("caller").expect("CPU work");
    }
    available.expect("cached pages, conditional reads, Markdown builds and uploads must not wait for parser CPU admission");
}
