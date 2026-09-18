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
    use docparse_core::{DocumentContext, DocumentResult, PageResult};
    use docparse_database::{
        connection, query::parse_job::ParseJobQuery as Jobs,
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
            db,
            storage,
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &Default::default(),
    )
    .expect("router");
    for suffix in ["&page=2", "&page=1", ""] {
        let uri = format!("/api/jobs/result?id={id}{suffix}");
        let response = app
            .clone()
            .oneshot(Request::get(&uri).body(Body::empty()).expect("request"))
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
}
