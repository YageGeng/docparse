use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use docparse_config::DatabaseConfig;
use docparse_config::{RawConfig, TableMode, ValidatedConfig};
use docparse_core::DocParser;
use docparse_database::{connection, query::parse_job::ParseJobQuery as Jobs};
use docparse_layout::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest,
    wasm_compat::WasmBoxedFuture,
};
use docparse_server::{
    app::router,
    code::ApiCode,
    error::{ApiError, PanicHandler, RequestSnafu},
    state::{AppState, HttpOptions},
    storage::SharedStorage,
    worker::{Worker, WorkerOptions},
};
use futures_util::StreamExt;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

/// Pauses the real parser at its existing engine boundary while HTTP clients disconnect.
struct GatedLayout {
    gate: Arc<Semaphore>,
    entered: mpsc::UnboundedSender<()>,
}

impl LayoutEngine for GatedLayout {
    /// Identifies deterministic layout fallback for the persistence acceptance test.
    fn name(&self) -> &str {
        "http-test"
    }
    /// Keeps repeated attempts on the same canonical model revision.
    fn model_revision(&self) -> &str {
        "http-test-v1"
    }
    /// Gates inference while preserving real PDF extraction and canonical page assembly.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async move {
            let _ = self.entered.send(());
            self.gate.acquire().await.expect("gate").forget();
            Ok(Vec::new())
        })
    }
}

/// Builds a multipart upload with an untrusted filename to check that storage names are server-owned.
fn upload(id: Uuid, pdf: &[u8]) -> Request<Body> {
    let mut body = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"../../untrusted.pdf\"\r\nContent-Type: application/pdf\r\n\r\n".to_vec();
    body.extend_from_slice(pdf);
    body.extend_from_slice(b"\r\n--boundary--\r\n");
    Request::post("/api/jobs")
        .header("content-type", "multipart/form-data; boundary=boundary")
        .header("idempotency-key", id.to_string())
        .body(Body::from(body))
        .expect("request")
}

/// Reads a bounded response from the actual Axum middleware and handler stack.
async fn json(app: Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("response");
    (
        status,
        serde_json::from_slice(&bytes).expect("JSON envelope"),
    )
}

/// Creates a bodyless GET request for polling the real API contract.
fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET")
}

/// Malformed workbench queries must fail at the HTTP boundary without reaching the database driver.
#[tokio::test]
async fn workbench_queries_reject_invalid_input() {
    let directory = tempfile::tempdir().expect("storage");
    let app = router(
        AppState::new(
            Default::default(),
            SharedStorage::new(directory.path()).await.expect("storage"),
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    for path in [
        "/api/jobs/list?limit=0",
        "/api/jobs/list?limit=101",
        "/api/jobs/list?cursor=bad",
        "/api/jobs/list?status=other",
        "/api/jobs/list?unknown=true",
        "/api/jobs/source?id=bad",
        "/api/jobs/result?id=00000000-0000-0000-0000-000000000000&page=0",
        "/api/jobs/result?id=00000000-0000-0000-0000-000000000000&page=-1",
        "/api/jobs/result?id=00000000-0000-0000-0000-000000000000&page=4294967296",
    ] {
        let (status, error) = json(app.clone(), get(path)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(
            error.pointer("/error/code").and_then(Value::as_u64),
            Some(4001002),
            "{path}"
        );
    }
}

/// Deletion cleans only one result, rejects live work, preserves cursors and shared PDFs, and can be retried after storage errors.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn completed_results_can_be_deleted_safely() {
    use docparse_database::{
        entities::parse_jobs,
        seaorm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr},
    };
    let db = connection::connect(
        &DatabaseConfig::builder()
            .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
            .build(),
    )
    .await
    .expect("database");
    let directory = tempfile::tempdir().expect("storage");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let app = router(
        AppState::new(
            db.clone(),
            storage.clone(),
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let id = Uuid::new_v4();
    let hash = "e".repeat(64);
    let source_path =
        storage.path(&format!("{hash}.pdf")).expect("source path");
    tokio::fs::write(&source_path, b"%PDF-shared")
        .await
        .expect("source");
    Jobs::submit(&db, id, &hash, Some("delete.pdf"), Some(11))
        .await
        .expect("submit");
    let lease = Jobs::claim(&db, 60, 3).await.expect("claim").expect("job");
    assert_eq!(lease.job.id, id);
    let result_name = format!("{id}-{}.json", lease.token);
    let result_path = storage.path(&result_name).expect("result path");
    Jobs::finish(
        &db,
        &lease,
        Ok(&result_name),
        None,
        Duration::from_millis(100),
    )
    .await
    .expect("finish");
    let sibling = Uuid::new_v4();
    Jobs::submit(&db, sibling, &hash, Some("same.pdf"), Some(11))
        .await
        .expect("shared source job");

    for (query, expected) in [
        ("bad".to_owned(), StatusCode::BAD_REQUEST),
        (Uuid::new_v4().to_string(), StatusCode::NOT_FOUND),
        (sibling.to_string(), StatusCode::CONFLICT),
    ] {
        assert_eq!(
            json(
                app.clone(),
                Request::post(format!("/api/jobs/delete?id={query}"))
                    .body(Body::empty())
                    .expect("delete request")
            )
            .await
            .0,
            expected
        );
    }
    // A directory in place of the result reliably causes an unlink failure, even when tests run as root.
    tokio::fs::create_dir(&result_path)
        .await
        .expect("blocked result");
    let path = format!("/api/jobs/delete?id={id}");
    assert_eq!(
        json(
            app.clone(),
            Request::post(&path).body(Body::empty()).expect("delete")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert!(
        Jobs::find_by_id(&db, id)
            .await
            .expect("read after failed deletion")
            .is_none(),
        "file cleanup failures must not roll back the durable deletion"
    );
    tokio::fs::remove_dir(&result_path)
        .await
        .expect("remove obstruction");
    tokio::fs::write(&result_path, b"{}").await.expect("result");
    let pending = parse_jobs::Entity::find_by_id(id)
        .one(&db)
        .await
        .expect("pending read")
        .expect("tombstone");
    assert!(pending.deleted_at.is_some());
    assert_eq!(pending.result_path.as_deref(), Some(result_name.as_str()));
    // A fresh connection and cleaner recover committed intent without the original HTTP request or process-local state.
    let recovered_state = AppState::new(
        connection::connect(
            &DatabaseConfig::builder()
                .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
                .build(),
        )
        .await
        .expect("replacement connection"),
        storage.clone(),
        HttpOptions::builder().build(),
        CancellationToken::new(),
    )
    .expect("replacement state");
    let cleanup_stop = CancellationToken::new();
    let cleaning = tokio::spawn(
        docparse_server::cleanup::DeletedResults::from(&recovered_state)
            .run(cleanup_stop.clone()),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if parse_jobs::Entity::find_by_id(id)
                .one(&db)
                .await
                .expect("cleanup status")
                .expect("tombstone")
                .result_path
                .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup cleanup completes");
    cleanup_stop.cancel();
    cleaning.await.expect("cleanup stops");
    assert!(!result_path.exists());
    // Recreate a process interrupted after unlink but before its database acknowledgement committed.
    parse_jobs::Entity::update_many()
        .col_expr(
            parse_jobs::Column::ResultPath,
            Expr::val(result_name.clone()),
        )
        .filter(parse_jobs::Column::Id.eq(id))
        .exec(&db)
        .await
        .expect("pending acknowledgement");
    docparse_server::cleanup::DeletedResults::from(&recovered_state)
        .clean(&pending)
        .await
        .expect("interrupted cleanup retry");
    assert!(
        parse_jobs::Entity::find_by_id(id)
            .one(&db)
            .await
            .expect("acknowledgement")
            .expect("tombstone")
            .result_path
            .is_none()
    );
    recovered_state
        .db
        .close()
        .await
        .expect("close replacement pool");
    let (first, repeated) = tokio::join!(
        json(
            app.clone(),
            Request::post(&path).body(Body::empty()).expect("delete")
        ),
        json(
            app.clone(),
            Request::post(&path)
                .body(Body::empty())
                .expect("repeat delete")
        )
    );
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(repeated.0, StatusCode::OK);
    assert!(!result_path.exists());
    assert_eq!(
        tokio::fs::read(&source_path).await.expect("shared PDF"),
        b"%PDF-shared"
    );
    assert!(
        Jobs::find_by_id(&db, sibling)
            .await
            .expect("sibling")
            .is_some()
    );
    assert!(
        Jobs::list(&db, Some(id), 20, None, None).await.is_ok(),
        "deleted cursor remains usable"
    );
    assert!(matches!(
        Jobs::submit(&db, id, &hash, None, None).await,
        Err(docparse_database::error::DatabaseError::IdempotencyConflict)
    ));
    for endpoint in ["status", "result", "source", "events"] {
        assert_eq!(
            json(app.clone(), get(&format!("/api/jobs/{endpoint}?id={id}")))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    let (_, history) = json(app, get("/api/jobs/list")).await;
    assert!(
        !history
            .pointer("/data/items")
            .and_then(Value::as_array)
            .expect("history")
            .iter()
            .any(|job| job.get("id").and_then(Value::as_str)
                == Some(&id.to_string()))
    );
    for id in [id, sibling] {
        parse_jobs::Entity::delete_by_id(id)
            .exec(&db)
            .await
            .expect("test cleanup");
    }
    db.close().await.expect("close database");
}

/// Upload, worker execution, cross-replica reads, SSE reconnect, and error envelopes use real PostgreSQL and PDFium.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn durable_http_survives_disconnected_clients() {
    let url =
        std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("database URL");
    let db = connection::connect(
        &DatabaseConfig::builder().url(url.clone()).build(),
    )
    .await
    .expect("database");
    let directory = tempfile::tempdir().expect("shared directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let shutdown = CancellationToken::new();
    let state = AppState::new(
        db.clone(),
        storage.clone(),
        HttpOptions::builder()
            .poll_interval(Duration::from_millis(20))
            .build(),
        shutdown.clone(),
    )
    .expect("state");
    let app = router(state.clone(), &docparse_config::ServerConfig::default())
        .expect("router");
    let pdf =
        include_bytes!("../../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let id = Uuid::new_v4();
    let accepted = app
        .clone()
        .oneshot(upload(id, pdf))
        .await
        .expect("upload response");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    // The acknowledgement points to the prefixed query endpoint that clients can actually poll.
    let location = accepted
        .headers()
        .get("location")
        .expect("status location")
        .to_str()
        .expect("URI")
        .to_owned();
    assert_eq!(location, format!("/api/jobs/status?id={id}"));
    let submitted: Value = serde_json::from_slice(
        &to_bytes(accepted.into_body(), 4096)
            .await
            .expect("submission body"),
    )
    .expect("submission JSON");
    // The workbench restores metadata and original bytes from the server after a browser refresh.
    assert_eq!(
        submitted.pointer("/data/filename").and_then(Value::as_str),
        Some("untrusted.pdf")
    );
    assert_eq!(
        submitted
            .pointer("/data/size_bytes")
            .and_then(Value::as_u64),
        Some(u64::try_from(pdf.len()).expect("PDF size"))
    );
    let history = json(
        app.clone(),
        get("/api/jobs/list?search=untrusted.pdf&limit=100"),
    )
    .await;
    assert_eq!(history.0, StatusCode::OK);
    assert!(
        history
            .1
            .pointer("/data/items")
            .and_then(Value::as_array)
            .expect("items")
            .iter()
            .any(|job| job.get("id").and_then(Value::as_str)
                == Some(id.to_string().as_str()))
    );
    let ranged = app
        .clone()
        .oneshot(
            Request::get(format!("/api/jobs/source?id={id}"))
                .header("range", "bytes=0-4")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .expect("range request"),
        )
        .await
        .expect("source range");
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert!(!ranged.headers().contains_key("content-encoding"));
    assert_eq!(
        ranged.headers().get("content-type").expect("content type"),
        "application/pdf"
    );
    assert_eq!(
        ranged
            .headers()
            .get("content-range")
            .expect("content range")
            .to_str()
            .expect("range text"),
        format!("bytes 0-4/{}", pdf.len())
    );
    assert_eq!(
        to_bytes(ranged.into_body(), 16)
            .await
            .expect("range bytes")
            .as_ref(),
        b"%PDF-"
    );
    let head = app
        .clone()
        .oneshot(
            Request::head(format!("/api/jobs/source?id={id}"))
                .body(Body::empty())
                .expect("HEAD request"),
        )
        .await
        .expect("source HEAD");
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        head.headers()
            .get("content-length")
            .expect("content length")
            .to_str()
            .expect("length text"),
        pdf.len().to_string()
    );
    assert!(
        to_bytes(head.into_body(), 16)
            .await
            .expect("HEAD body")
            .is_empty()
    );
    let invalid_range = app
        .clone()
        .oneshot(
            Request::get(format!("/api/jobs/source?id={id}"))
                .header("range", "bytes=999999999-")
                .body(Body::empty())
                .expect("range request"),
        )
        .await
        .expect("range response");
    assert_eq!(invalid_range.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(json(app.clone(), get(&location)).await.0, StatusCode::OK);
    // A replica mounted elsewhere must return its own usable Location, including on an idempotent retry.
    let custom = docparse_config::ServerConfig::builder()
        .api_prefix("/service/docparse/v2")
        .build();
    let mounted = router(state.clone(), &custom).expect("custom prefix");
    let mut retry = upload(id, pdf);
    *retry.uri_mut() = "/service/docparse/v2/jobs".parse().expect("upload URI");
    let retried = mounted
        .clone()
        .oneshot(retry)
        .await
        .expect("prefixed retry");
    assert_eq!(retried.status(), StatusCode::ACCEPTED);
    let location = retried
        .headers()
        .get("location")
        .expect("retry location")
        .to_str()
        .expect("URI");
    assert_eq!(
        location,
        format!("/service/docparse/v2/jobs/status?id={id}")
    );
    assert_eq!(json(mounted, get(location)).await.0, StatusCode::OK);
    assert_eq!(
        submitted.get("data").expect("data").get("id").expect("id"),
        &serde_json::Value::String(id.to_string())
    );
    assert!(
        submitted
            .get("data")
            .expect("data")
            .get("lease_token")
            .is_none()
    );
    let gate = Arc::new(Semaphore::new(0));
    let (entered, mut receiver) = mpsc::unbounded_channel();
    let mut config = RawConfig::default();
    config.formula.inline_enabled = false;
    config.formula.display_enabled = false;
    config.tsr.mode = TableMode::RulesOnly;
    let config = Arc::new(ValidatedConfig::try_from(config).expect("config"));
    let parser = DocParser::builder()
        .config(Arc::clone(&config))
        .layout_engine(Arc::new(GatedLayout {
            gate: Arc::clone(&gate),
            entered,
        }))
        .build()
        .await
        .expect("parser");
    let worker = Worker::builder()
        .db(db.clone())
        .storage(storage.clone())
        .parser(Arc::new(parser))
        .output(config.output().clone())
        .options(
            WorkerOptions::builder()
                .concurrency(1)
                .lease_seconds(3)
                .build(),
        )
        .build();
    let worker_stop = CancellationToken::new();
    let worker_signal = worker_stop.clone();
    let running_worker = worker.clone();
    let task =
        tokio::spawn(async move { running_worker.run(worker_signal).await });
    tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .expect("worker starts");
    let events_path = format!("/api/jobs/events?id={id}");
    let stream = app
        .clone()
        .oneshot(get(&events_path))
        .await
        .expect("subscribe");
    assert_eq!(
        stream
            .headers()
            .get("x-accel-buffering")
            .expect("x-accel-buffering"),
        "no"
    );
    let mut stream = stream.into_body().into_data_stream();
    let snapshot = stream.next().await.expect("snapshot").expect("data");
    assert!(String::from_utf8_lossy(&snapshot).contains("event: job"));
    drop(stream);
    // Keep the parser alive longer than its initial lease, proving renewal is independent of SSE.
    tokio::time::sleep(Duration::from_millis(3300)).await;
    assert!(
        Jobs::claim(&db, 3, 3)
            .await
            .expect("no duplicate claim")
            .is_none()
    );
    worker_stop.cancel();
    assert!(
        !task.is_finished(),
        "rolling shutdown must drain the active attempt"
    );
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("drain")
        .expect("join")
        .expect("worker");
    let other_db = connection::connect(
        &DatabaseConfig::builder().url(url.clone()).build(),
    )
    .await
    .expect("second replica connection");
    let other = router(
        AppState::new(
            other_db,
            storage,
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("second replica"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let (_, completed) =
        json(other.clone(), get(&format!("/api/jobs/status?id={id}"))).await;
    assert_eq!(
        completed
            .get("data")
            .expect("data")
            .get("status")
            .expect("status"),
        "succeeded"
    );
    assert_eq!(
        completed
            .get("data")
            .expect("data")
            .get("attempts")
            .expect("attempts"),
        1
    );
    assert_eq!(
        completed
            .pointer("/data/progress/stage")
            .and_then(Value::as_str),
        Some("complete"),
        "terminal state and progress must be published together"
    );
    // The persisted duration must include the deliberate layout delay and survive reading from another API replica.
    let duration_ms = completed
        .pointer("/data/duration_ms")
        .and_then(Value::as_u64)
        .expect("persisted parse duration");
    assert!(duration_ms >= 3300, "duration excludes parser work");
    let (status, result) =
        json(other.clone(), get(&format!("/api/jobs/result?id={id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result.get("success").expect("success"), true);
    assert_eq!(
        result
            .get("data")
            .expect("data")
            .get("pages")
            .expect("pages")
            .as_array()
            .expect("pages")
            .len(),
        1
    );
    let terminal = other
        .clone()
        .oneshot(get(&events_path))
        .await
        .expect("reconnect");
    let terminal = to_bytes(terminal.into_body(), 64 * 1024)
        .await
        .expect("terminal closes");
    assert!(String::from_utf8_lossy(&terminal).contains("succeeded"));
    let terminal_text = String::from_utf8_lossy(&terminal);
    let snapshot: Value = serde_json::from_str(
        terminal_text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("terminal SSE data"),
    )
    .expect("SSE snapshot");
    assert_eq!(
        snapshot
            .pointer("/data/duration_ms")
            .and_then(Value::as_u64),
        Some(duration_ms)
    );
    let (_, history) = json(other.clone(), get("/api/jobs/list")).await;
    let entry = history
        .pointer("/data/items")
        .and_then(Value::as_array)
        .expect("history")
        .iter()
        .find(|entry| {
            entry.get("id").and_then(Value::as_str) == Some(&id.to_string())
        })
        .expect("completed history entry");
    assert_eq!(
        entry.get("duration_ms").and_then(Value::as_u64),
        Some(duration_ms)
    );
    assert_eq!(
        json(other.clone(), upload(id, pdf))
            .await
            .1
            .get("data")
            .expect("data")
            .get("status")
            .expect("status"),
        "succeeded"
    );
    // A killed worker loses its lease; a replacement must recover the same persisted job ID.
    let recovery_id = Uuid::new_v4();
    assert_eq!(
        json(other.clone(), upload(recovery_id, pdf)).await.0,
        StatusCode::ACCEPTED
    );
    let abandoned = Jobs::claim(&db, 3, 3)
        .await
        .expect("claim recovery job")
        .expect("queued job");
    let lost_worker = worker.clone();
    let lost =
        tokio::spawn(async move { lost_worker.process(abandoned).await });
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("abandoned attempt entered layout");
    lost.abort();
    assert!(lost.await.expect_err("worker aborted").is_cancelled());
    tokio::time::sleep(Duration::from_millis(3400)).await;
    let replacement = Jobs::claim(&db, 3, 3)
        .await
        .expect("recover")
        .expect("expired job");
    assert_eq!(replacement.job.id, recovery_id);
    assert_eq!(replacement.job.attempts, 2);
    gate.add_permits(1);
    worker
        .process(replacement)
        .await
        .expect("replacement finishes");
    assert_eq!(
        json(
            other.clone(),
            get(&format!("/api/jobs/status?id={recovery_id}"))
        )
        .await
        .1
        .get("data")
        .expect("data")
        .get("status")
        .expect("status"),
        "succeeded"
    );
    let (status, conflict) =
        json(other.clone(), upload(id, b"%PDF-different")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        conflict
            .get("error")
            .expect("error")
            .get("code")
            .expect("code"),
        4091001
    );
    for (path, expected) in [
        ("/missing", ApiCode::COMMON_NOT_FOUND),
        (
            "/api/jobs/status?id=not-a-uuid",
            ApiCode::bad_request(4001002),
        ),
    ] {
        let (status, error) = json(other.clone(), get(path)).await;
        assert_eq!(status, expected.http_code);
        assert_eq!(
            error
                .get("error")
                .expect("error")
                .get("code")
                .expect("code"),
            expected.code
        );
    }
    let (status, error) =
        json(other.clone(), upload(Uuid::new_v4(), b"not a PDF")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error
            .get("error")
            .expect("error")
            .get("code")
            .expect("code"),
        4001001
    );
    let tiny = router(
        AppState::new(
            state.db.clone(),
            state.storage.clone(),
            HttpOptions::builder().max_upload_bytes(4).build(),
            shutdown.clone(),
        )
        .expect("tiny limit"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let (status, error) = json(tiny, upload(Uuid::new_v4(), pdf)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        error
            .get("error")
            .expect("error")
            .get("code")
            .expect("code"),
        4131001
    );
    shutdown.cancel();
    assert_eq!(
        json(app, get("/api/ready"))
            .await
            .1
            .get("error")
            .expect("error")
            .get("code")
            .expect("code"),
        5031001
    );
    // Persist the parser's Display message, including its stage, when a real PDFium open fails.
    let failed_id = Uuid::new_v4();
    assert_eq!(
        json(other.clone(), upload(failed_id, b"%PDF-invalid-document"))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let failed_lease = Jobs::claim(&db, 60, 1)
        .await
        .expect("claim")
        .expect("failed input job");
    assert_eq!(failed_lease.job.id, failed_id);
    worker
        .process(failed_lease)
        .await
        .expect("record parse failure");
    // Failed attempts requeue first; the next claim marks exhausted retries terminal without replacing their message.
    assert!(
        Jobs::claim(&db, 60, 1)
            .await
            .expect("reap exhausted retry")
            .is_none()
    );
    let (_, failed) =
        json(other, get(&format!("/api/jobs/status?id={failed_id}"))).await;
    assert_eq!(
        failed.pointer("/data/status").and_then(Value::as_str),
        Some("failed")
    );
    assert!(
        failed
            .pointer("/data/duration_ms")
            .and_then(Value::as_u64)
            .is_some(),
        "failed attempts retain their duration"
    );
    assert!(
        failed
            .pointer("/data/error")
            .and_then(Value::as_str)
            .expect("persisted message")
            .starts_with("parser failed at document-parse-pdf:")
    );
}

/// A failure after SSE starts must serialize the same Display-based error envelope and then close the stream.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn sse_failure_uses_the_error_code_trait() {
    use docparse_database::{entities::parse_jobs, seaorm::EntityTrait};
    let db = connection::connect(
        &DatabaseConfig::builder()
            .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
            .build(),
    )
    .await
    .expect("database");
    let directory = tempfile::tempdir().expect("storage");
    let app = router(
        AppState::new(
            db.clone(),
            SharedStorage::new(directory.path()).await.expect("storage"),
            HttpOptions::builder()
                .poll_interval(Duration::from_millis(10))
                .build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let id = Uuid::new_v4();
    Jobs::submit(&db, id, &"a".repeat(64), None, None)
        .await
        .expect("submit");
    let response = app
        .clone()
        .oneshot(get(&format!("/api/jobs/events?id={id}")))
        .await
        .expect("SSE");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("content-encoding"));
    let mut request = get(&format!("/api/jobs/events?id={id}"));
    request
        .headers_mut()
        .insert("accept-encoding", "gzip".parse().expect("header"));
    let second = app.oneshot(request).await.expect("second SSE");
    assert!(!second.headers().contains_key("content-encoding"));
    let mut second = second.into_body().into_data_stream();
    second.next().await.expect("second replay").expect("frame");
    let mut stream = response.into_body().into_data_stream();
    stream
        .next()
        .await
        .expect("initial snapshot")
        .expect("frame");
    parse_jobs::Entity::delete_by_id(id)
        .exec(&db)
        .await
        .expect("remove observed task");
    let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("poll")
        .expect("error event")
        .expect("frame");
    let frame = String::from_utf8_lossy(&frame);
    assert!(frame.contains("event: error"));
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("SSE JSON");
    assert_eq!(
        serde_json::from_str::<Value>(data).expect("error envelope"),
        serde_json::json!({"success":false,"error":{"code":4041001,"message":"request failed at job-events-poll"}})
    );
    assert!(stream.next().await.is_none());
    let second_error =
        tokio::time::timeout(Duration::from_secs(5), second.next())
            .await
            .expect("second poll")
            .expect("error")
            .expect("frame");
    assert!(String::from_utf8_lossy(&second_error).contains("event: error"));
    assert!(second.next().await.is_none());
}

/// Caught panics use the typed error envelope without copying the arbitrary panic payload into Display.
#[tokio::test]
#[allow(
    clippy::panic,
    reason = "verifies the HTTP catch-panic boundary with a real handler unwind"
)]
async fn errors_and_panics_use_api_codes() {
    let app = Router::new()
        .route(
            "/",
            axum::routing::get(|| async {
                panic!("private diagnostic");
                #[allow(unreachable_code)]
                RequestSnafu {
                    stage: "test-handler-panic",
                    code: ApiCode::COMMON_INTERNAL_ERROR,
                }
                .build()
            }),
        )
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            PanicHandler,
        ));
    let (status, response) = json(app, get("/")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.get("success").expect("success"), false);
    assert_eq!(
        response
            .get("error")
            .expect("error")
            .get("code")
            .expect("code"),
        ApiCode::COMMON_INTERNAL_ERROR.code
    );
    assert!(!response.to_string().contains("private diagnostic"));
}

/// Every source-bearing error returns its explicit code and Display message through the same response conversion.
#[tokio::test]
async fn error_variants_preserve_explicit_codes() {
    let code = ApiCode::too_many_requests(4291001);
    let task = tokio::spawn(std::future::pending::<()>());
    task.abort();
    let errors = [
        ApiError::Database {
            source: Box::new(
                docparse_database::error::DatabaseError::IdempotencyConflict,
            ),
            stage: "test-private-stage",
            code,
        },
        ApiError::Storage {
            source: std::io::Error::other("private diagnostic"),
            stage: "test-private-stage",
            code,
        },
        ApiError::Task {
            stage: "test-join-task",
            source: task.await.expect_err("cancelled"),
            code,
        },
        ApiError::Serialize {
            stage: "test-serialize-json",
            source: serde_json::from_str::<Value>("{")
                .expect_err("invalid JSON"),
            code,
        },
        ApiError::Parse {
            stage: "test-parse-pdf",
            source: Box::new(
                docparse_core::DocParseError::MissingConfiguration,
            ),
            code,
        },
    ];
    for error in errors {
        assert!(std::error::Error::source(&error).is_some());
        let message = error.to_string();
        let response = error.into_response();
        assert_eq!(response.status(), code.http_code);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("error body");
        let body: Value = serde_json::from_slice(&bytes).expect("error JSON");
        assert_eq!(
            body,
            serde_json::json!({"success":false,"error":{"code":code.code,"message":message}})
        );
    }
}

/// Router and extractor failures enter the typed error boundary directly, preserving method headers and upload limits.
#[tokio::test]
async fn routing_and_upload_rejections_use_api_errors() {
    let directory = tempfile::tempdir().expect("storage");
    let app = router(
        AppState::new(
            Default::default(),
            SharedStorage::new(directory.path()).await.expect("storage"),
            HttpOptions::builder().max_upload_bytes(4).build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("method rejection");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers().contains_key("allow"));
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("method body");
    let body: Value = serde_json::from_slice(&body).expect("method JSON");
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(405000)
    );
    for (request, code) in [
        (get("/missing"), ApiCode::COMMON_NOT_FOUND),
        (
            get("/api/jobs/status?id=not-a-uuid"),
            ApiCode::bad_request(4001002),
        ),
        (
            Request::post("/api/jobs")
                .header("idempotency-key", Uuid::new_v4().to_string())
                .body(Body::empty())
                .expect("missing boundary"),
            ApiCode::COMMON_BAD_REQUEST,
        ),
        (
            upload(Uuid::new_v4(), b"bad"),
            ApiCode::bad_request(4001001),
        ),
        (
            upload(Uuid::new_v4(), b"%PDF-too-large"),
            ApiCode::payload_too_large(4131001),
        ),
        (
            upload(Uuid::new_v4(), &vec![b'x'; 70 * 1024]),
            ApiCode::payload_too_large(4131001),
        ),
    ] {
        let (status, body) = json(app.clone(), request).await;
        assert_eq!(status, code.http_code);
        assert_eq!(
            body.pointer("/error/code").and_then(Value::as_u64),
            Some(code.code)
        );
    }
}

/// Query rejections preserve API codes without touching the database, and legacy capture routes remain absent.
#[tokio::test]
async fn job_queries_reject_missing_invalid_and_duplicate_identifiers() {
    let directory = tempfile::tempdir().expect("storage");
    let app = router(
        AppState::new(
            Default::default(),
            SharedStorage::new(directory.path()).await.expect("storage"),
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let id = Uuid::new_v4();
    for endpoint in ["status", "events", "result"] {
        for query in [
            String::new(),
            "?id=bad".into(),
            format!("?id={id}&id={id}"),
            format!("?id={id}&unexpected=true"),
        ] {
            let (status, body) =
                json(app.clone(), get(&format!("/api/jobs/{endpoint}{query}")))
                    .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{endpoint}{query}");
            assert_eq!(
                body.pointer("/error/code").and_then(Value::as_u64),
                Some(4001002)
            );
        }
    }
    for path in [
        format!("/api/jobs/{id}"),
        format!("/api/jobs/{id}/events"),
        format!("/api/jobs/{id}/result"),
    ] {
        assert_eq!(
            json(app.clone(), get(&path)).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

/// Database connectivity alone must not mark an API ready when its task schema is absent.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn readiness_requires_the_task_schema() {
    use docparse_database::seaorm::{ConnectOptions, Database};
    let mut options = ConnectOptions::new(
        std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"),
    );
    options
        .set_schema_search_path(format!("absent_{}", Uuid::new_v4().simple()))
        .sqlx_logging(false);
    let db = Database::connect(options)
        .await
        .expect("connected database with absent schema");
    let directory = tempfile::tempdir().expect("storage");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let app = router(
        AppState::new(
            db,
            storage,
            HttpOptions::builder().build(),
            CancellationToken::new(),
        )
        .expect("state"),
        &docparse_config::ServerConfig::default(),
    )
    .expect("router");
    let (status, body) = json(app, get("/api/ready")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body.pointer("/error/code").and_then(Value::as_u64),
        Some(ApiCode::COMMON_DATABASE_ERROR.code)
    );
}

/// Simulates a synchronous parser phase without stalling the separate test/lease-check executor thread.
struct BlockingIdentity {
    entered: mpsc::UnboundedSender<()>,
    first: std::sync::atomic::AtomicBool,
}

impl LayoutEngine for BlockingIdentity {
    /// The parser calls engine metadata synchronously on its own future, like document context and final validation work.
    fn name(&self) -> &str {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = self.entered.send(());
            std::thread::sleep(Duration::from_secs(4));
        }
        "blocking-identity"
    }
    /// Supplies a stable model revision for the real PDF pipeline.
    fn model_revision(&self) -> &str {
        "review-test"
    }
    /// Retains the real native geometry fallback after the deliberately expensive synchronous phase.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// Synchronous parser work must not stop the lease supervisor and cause valid work to be reclaimed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn worker_renews_during_synchronous_parser_work() {
    use docparse_database::{entities::parse_jobs, seaorm::EntityTrait};
    let db = connection::connect(
        &DatabaseConfig::builder()
            .url(std::env::var("DOCPARSE_TEST_DATABASE_URL").expect("URL"))
            .build(),
    )
    .await
    .expect("db");
    let directory = tempfile::tempdir().expect("directory");
    let storage = SharedStorage::new(directory.path()).await.expect("storage");
    let pdf =
        include_bytes!("../../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let hash = blake3::hash(pdf).to_hex().to_string();
    tokio::fs::write(storage.path(&format!("{hash}.pdf")).expect("path"), pdf)
        .await
        .expect("PDF");
    let id = Uuid::new_v4();
    Jobs::submit(&db, id, &hash, None, None)
        .await
        .expect("submit");
    let lease = Jobs::claim(&db, 3, 3).await.expect("claim").expect("job");
    assert_eq!(lease.job.id, id);
    let (entered, mut receiver) = mpsc::unbounded_channel();
    let mut raw = RawConfig::default();
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.tsr.mode = TableMode::RulesOnly;
    let config = Arc::new(ValidatedConfig::try_from(raw).expect("config"));
    let parser = DocParser::builder()
        .config(Arc::clone(&config))
        .layout_engine(Arc::new(BlockingIdentity {
            entered,
            first: std::sync::atomic::AtomicBool::new(true),
        }))
        .build()
        .await
        .expect("parser");
    let worker = Worker::builder()
        .db(db.clone())
        .storage(storage)
        .parser(Arc::new(parser))
        .output(config.output().clone())
        .options(WorkerOptions::builder().lease_seconds(3).build())
        .build();
    let task = tokio::spawn(async move { worker.process(lease).await });
    receiver.recv().await.expect("synchronous phase started");
    tokio::time::sleep(Duration::from_millis(3300)).await;
    let duplicate = Jobs::claim(&db, 3, 3).await.expect("inspect lease");
    task.await.expect("join").expect("worker");
    let result = Jobs::find_by_id(&db, id)
        .await
        .expect("read job")
        .expect("job");
    parse_jobs::Entity::delete_by_id(id)
        .exec(&db)
        .await
        .expect("cleanup own job");
    assert!(
        duplicate.is_none(),
        "synchronous parser work must not let the lease expire"
    );
    assert_eq!(result.status, docparse_database::JobStatus::Succeeded);
}
