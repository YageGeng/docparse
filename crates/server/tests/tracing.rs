use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use docparse_config::{DatabaseConfig, RawConfig, TableMode, ValidatedConfig};
use docparse_core::DocParser;
use docparse_database::{
    connection,
    entities::parse_jobs,
    query::parse_job::ParseJobQuery as Jobs,
    seaorm::{
        ConnectionTrait, EntityTrait,
        sea_query::{Alias, Expr, Func, Query},
    },
};
use docparse_layout::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest,
    wasm_compat::{SessionWorker, TaskError, WasmBoxedFuture, run_cpu},
};
use docparse_server::{
    app::router,
    state::{AppState, HttpOptions},
    storage::SharedStorage,
    worker::{Worker, WorkerOptions},
};
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing::{Instrument, instrument::WithSubscriber};
use uuid::Uuid;

/// Streaming telemetry must wait for consumption and distinguish cancellation from a completed response.
#[tokio::test]
async fn streaming_logs_follow_body_lifecycle() {
    let log = tempfile::NamedTempFile::new().expect("log");
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(log.reopen().expect("writer"))
        .finish();
    async {
        let app = axum::Router::new()
            .route(
                "/stream",
                axum::routing::get(|| async {
                    Body::from_stream(futures_util::stream::iter([Ok::<
                        _,
                        std::io::Error,
                    >(
                        "payload"
                    )]))
                }),
            )
            .layer(axum::middleware::from_fn(
                docparse_server::middlewares::trace::request_trace,
            ));
        let response = app
            .clone()
            .oneshot(
                Request::get("/stream")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert!(
            !std::fs::read_to_string(log.path())
                .expect("logs")
                .contains("body completed")
        );
        assert_eq!(
            to_bytes(response.into_body(), 100).await.expect("body"),
            "payload"
        );
        let text = std::fs::read_to_string(log.path()).expect("logs");
        assert!(text.contains("body completed"), "{text}");
        assert!(text.contains("7 bytes"), "{text}");
        drop(
            app.oneshot(
                Request::get("/stream")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response"),
        );
        assert!(
            std::fs::read_to_string(log.path())
                .expect("logs")
                .contains("body cancelled")
        );
    }
    .with_subscriber(subscriber)
    .await;
}

/// File logging must append plain text across restarts while keeping stdout active.
#[test]
fn file_subscriber_appends_plaintext() {
    // Isolate RUST_LOG without mutating the environment of concurrently running tests.
    if std::env::var_os("DOCPARSE_TEST_LOG_SUBPROCESS").is_none() {
        let output = std::process::Command::new(
            std::env::current_exe().expect("test executable"),
        )
        .args([
            "--exact",
            "file_subscriber_appends_plaintext",
            "--nocapture",
        ])
        .env("DOCPARSE_TEST_LOG_SUBPROCESS", "1")
        .env("RUST_LOG", "info")
        // Capture stdout to catch file configuration accidentally replacing console output.
        .output()
        .expect("isolated logging test");
        assert!(
            output.status.success(),
            "logging subprocess failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
        assert_eq!(stdout.matches("file-logging-probe").count(), 2);
        assert_eq!(stdout.matches("trace_id=").count(), 2);
        assert!(
            !stdout.contains('\u{1b}'),
            "redirected stdout has ANSI escapes"
        );
        return;
    }
    let directory = tempfile::tempdir().expect("log directory");
    let path = directory.path().join("nested/server.log");
    let config = docparse_config::LogConfig::builder()
        .directives("info")
        .file(path.clone())
        .build();
    for _ in 0..2 {
        let subscriber = docparse_server::logging::subscriber(&config)
            .expect("file subscriber");
        tracing::subscriber::with_default(subscriber, || {
            // Recording a field later must not duplicate it through both formatters' span caches.
            let span = tracing::info_span!(
                "file_logging",
                trace_id = tracing::field::Empty
            );
            span.record("trace_id", "file-trace");
            span.in_scope(|| tracing::info!("file-logging-probe"));
        });
    }
    let output = std::fs::read_to_string(path).expect("persisted logs");
    assert_eq!(output.matches("file-logging-probe").count(), 2);
    assert_eq!(output.matches("trace_id=").count(), 2);
    assert!(!output.contains('\u{1b}'), "file contains ANSI escapes");
    assert!(output.contains("INFO"));
}

/// Database logging must apply both configured levels and the threshold, including independent off switches.
#[tokio::test]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn database_sql_logging_honors_configuration() {
    for (ordinary, slow, threshold_ms, expected_level) in [
        ("info", "off", 60_000, Some("INFO")),
        ("off", "warn", 1, Some("WARN")),
        ("off", "off", 1, None),
    ] {
        let log = tempfile::NamedTempFile::new().expect("log");
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_env_filter("off,sqlx::query=trace")
            .with_writer(log.reopen().expect("writer"))
            .finish();
        async {
            let db = connection::connect(
                &DatabaseConfig::builder()
                    .url(
                        std::env::var("DOCPARSE_TEST_DATABASE_URL")
                            .expect("URL"),
                    )
                    .sqlx_logging_level(
                        ordinary.parse().expect("ordinary level"),
                    )
                    .sqlx_slow_statements_logging_level(
                        slow.parse().expect("slow level"),
                    )
                    .sqlx_slow_statements_threshold_ms(threshold_ms)
                    .build(),
            )
            .await
            .expect("database");
            // A server-side delay reliably crosses the slow threshold without depending on network latency.
            db.query_one(
                &Query::select()
                    .expr_as(
                        Func::cust("pg_sleep").arg(Expr::val(0.03_f64)),
                        Alias::new("sql_logging_probe"),
                    )
                    .to_owned(),
            )
            .await
            .expect("query");
            db.close().await.expect("close database");
        }
        .with_subscriber(subscriber)
        .await;
        let output = std::fs::read_to_string(log.path()).expect("logs");
        let event = output
            .lines()
            .find(|line| line.contains("sql_logging_probe"));
        if let Some(level) = expected_level {
            let event = event.expect("SQL query event");
            assert!(event.contains(level), "unexpected level: {event}");
            assert_eq!(event.contains("slow statement"), threshold_ms == 1);
        } else {
            assert!(event.is_none(), "disabled query logging: {output}");
        }
    }
}

/// Startup must honor configured filters and RUST_LOG precedence before attempting any database connection.
#[test]
fn startup_uses_configured_log_directives() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = directory.path().join("docparse.toml");
    for (configured, environment, expected_error) in [
        ("docparse_server=invalid-level", None, "ParseError"),
        (
            "docparse_server=invalid-level",
            Some("warn"),
            "database.url",
        ),
        (
            "info",
            Some("docparse_server=invalid-level"),
            "database.url",
        ),
    ] {
        // An empty database URL stops valid logging setups immediately, keeping this test independent of services and models.
        std::fs::write(&path, format!("[log]\ndirectives = {configured:?}\n"))
            .expect("configuration");
        let mut command =
            std::process::Command::new(env!("CARGO_BIN_EXE_docparse-server"));
        command
            .env_clear()
            .env(
                "LD_LIBRARY_PATH",
                std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
            )
            .args(["--role", "api", "--config"])
            .arg(&path);
        if let Some(directives) = environment {
            command.env("RUST_LOG", directives);
        }
        let output = command.output().expect("server startup");
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(expected_error),
            "unexpected startup error: {error}"
        );
    }
}

/// Shared native workers must restore each caller's local subscriber without sending logs to another caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_helpers_keep_scoped_subscribers_separate() {
    let logs = [
        tempfile::NamedTempFile::new().expect("log A"),
        tempfile::NamedTempFile::new().expect("log B"),
    ];
    let dispatches = logs.each_ref().map(|log| {
        tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(log.reopen().expect("writer"))
                .finish(),
        )
    });
    let session = Arc::new(
        async {
            SessionWorker::new(|| {
                tracing::warn!("probe-session-initialize");
                Ok::<_, TaskError>(())
            })
            .await
            .expect("session")
        }
        .with_subscriber(dispatches.first().expect("first dispatcher").clone())
        .await,
    );
    let mut tasks = tokio::task::JoinSet::new();
    for (id, dispatch) in ["caller-a", "caller-b"].into_iter().zip(dispatches) {
        let session = Arc::clone(&session);
        tasks.spawn(
            async move {
                let span = tracing::info_span!("local_job", job_id = id);
                async move {
                    tracing::warn!("probe-scoped-caller");
                    run_cpu(|| tracing::warn!("probe-scoped-cpu"))
                        .await
                        .expect("CPU");
                    session
                        .run(|_| tracing::warn!("probe-scoped-session"))
                        .await
                        .expect("native session");
                }
                .instrument(span)
                .await;
            }
            .with_subscriber(dispatch),
        );
    }
    while let Some(task) = tasks.join_next().await {
        task.expect("caller");
    }
    for ((id, other), log) in
        [("caller-a", "caller-b"), ("caller-b", "caller-a")]
            .into_iter()
            .zip(logs)
    {
        let output = std::fs::read_to_string(log.path()).expect("logs");
        for event in [
            "probe-scoped-caller",
            "probe-scoped-cpu",
            "probe-scoped-session",
        ] {
            assert!(
                output
                    .lines()
                    .any(|line| line.contains(event) && line.contains(id)),
                "missing {event} for {id}: {output}"
            );
        }
        assert!(!output.contains(other), "subscriber leaked: {output}");
        assert_eq!(
            output.contains("probe-session-initialize"),
            id == "caller-a"
        );
    }
}

/// Log-level and module filters must keep correlation fields without enabling ordinary INFO events inside those spans.
#[tokio::test]
async fn correlation_spans_survive_event_filters() {
    for directive in ["warn", "error", "docparse_core=debug"] {
        let log = tempfile::NamedTempFile::new().expect("log");
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_env_filter(docparse_server::logging::filter(
                tracing_subscriber::EnvFilter::new(directive),
            ))
            .with_writer(log.reopen().expect("writer"))
            .finish();
        async {
            let span = tracing::info_span!(target: "docparse::context", "pdf_parse", job_id = "filter-job", pdf_hash = "filter-hash", attempt = 2);
            async {
                tracing::error!(target: "docparse_core", "visible-error");
                tracing::warn!("level-dependent-warning");
                tracing::info!("hidden-info");
            }.instrument(span).await;
        }.with_subscriber(subscriber).await;
        let output = std::fs::read_to_string(log.path()).expect("logs");
        let error = output
            .lines()
            .find(|line| line.contains("visible-error"))
            .expect("error event");
        assert!(
            error.contains("filter-job")
                && error.contains("filter-hash")
                && error.contains("attempt=2"),
            "missing correlation: {output}"
        );
        assert!(
            !output.contains("hidden-info"),
            "span target enabled child INFO: {output}"
        );
        assert_eq!(
            output.contains("level-dependent-warning"),
            directive == "warn"
        );
    }
}

/// Exercises real page scheduling, CPU dispatch, and the shared native session actor without requiring model artifacts.
struct ProbeLayout {
    barrier: Arc<Barrier>,
    session: Arc<SessionWorker<()>>,
}

impl LayoutEngine for ProbeLayout {
    /// Identifies the injected engine in existing parser lifecycle logs.
    fn name(&self) -> &str {
        "trace-probe"
    }
    /// Keeps canonical document metadata deterministic.
    fn model_revision(&self) -> &str {
        "trace-probe-v1"
    }
    /// Forces two PDFs to overlap before emitting events on each real execution boundary.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async move {
            self.barrier.wait().await;
            tracing::info!("probe-layout-async");
            run_cpu(|| tracing::info!("probe-layout-cpu"))
                .await
                .expect("CPU dispatch");
            self.session
                .run(|_| tracing::info!("probe-layout-session"))
                .await
                .expect("session dispatch");
            Ok(Vec::new())
        })
    }
}

/// Concurrent PDFs, HTTP reads, native work and retries retain the same durable identifiers without sharing another job's span.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires DOCPARSE_TEST_DATABASE_URL pointing at a disposable PostgreSQL database"]
async fn pdf_logs_follow_jobs_across_execution_boundaries() {
    let log = tempfile::NamedTempFile::new().expect("log file");
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_env_filter(docparse_server::logging::filter(
            tracing_subscriber::EnvFilter::new("debug"),
        ))
        .with_writer(log.reopen().expect("log writer"))
        .finish();
    // Exercise the full parser with a scoped dispatcher instead of relying on a process-global subscriber.
    async {
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
            .expect("state"), &docparse_config::ServerConfig::default()).expect("router");
        let mut raw = RawConfig::default();
        raw.formula.inline_enabled = false;
        raw.formula.display_enabled = false;
        raw.tsr.mode = TableMode::RulesOnly;
        let config = Arc::new(ValidatedConfig::try_from(raw).expect("config"));
        let parser = DocParser::builder()
            .config(Arc::clone(&config))
            .layout_engine(Arc::new(ProbeLayout {
                barrier: Arc::new(Barrier::new(2)),
                session: Arc::new(
                    SessionWorker::new(|| Ok::<_, TaskError>(()))
                        .await
                        .expect("session"),
                ),
            }))
            .build()
            .await
            .expect("parser");
        let worker = Worker::builder()
            .db(db.clone())
            .storage(storage)
            .parser(Arc::new(parser))
            .output(config.output().clone())
            .options(WorkerOptions::builder().build())
            .build();
        let ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let pdf =
            include_bytes!("../../core/tests/fixtures/pdf/extraction_metadata.pdf");
        let invalid = b"%PDF-invalid-tracing-probe";
        let hash = blake3::hash(pdf).to_hex().to_string();
        for (id, bytes) in
            ids.iter()
                .zip([pdf.as_slice(), pdf.as_slice(), invalid.as_slice()])
        {
            let mut body = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"probe.pdf\"\r\n\r\n".to_vec();
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n--boundary--\r\n");
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/jobs")
                        .header("idempotency-key", id.to_string())
                        .header(
                            "content-type",
                            "multipart/form-data; boundary=boundary",
                        )
                        .body(Body::from(body))
                        .expect("upload"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert!(response.headers().contains_key("x-request-id"));
        }
        let first = Jobs::claim(&db, 60, 2)
            .await
            .expect("claim")
            .expect("first");
        let second = Jobs::claim(&db, 60, 2)
            .await
            .expect("claim")
            .expect("second");
        let [first_id, second_id, invalid_id] = ids;
        assert_eq!(first.job.id, first_id);
        assert_eq!(second.job.id, second_id);
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::try_join!(worker.process(first), worker.process(second))
        })
        .await
        .expect("concurrent parses")
        .expect("workers");
        for attempt in 1..=2 {
            let lease = Jobs::claim(&db, 60, 2)
                .await
                .expect("claim")
                .expect("invalid PDF");
            assert_eq!(lease.job.id, invalid_id);
            assert_eq!(lease.job.attempts, attempt);
            worker.process(lease).await.expect("persist failure");
        }
        assert!(
            Jobs::claim(&db, 60, 2)
                .await
                .expect("reap exhausted retry")
                .is_none()
        );
        for id in ids {
            for endpoint in ["status", "events", "result"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::get(format!("/api/jobs/{endpoint}?id={id}"))
                            .body(Body::empty())
                            .expect("GET"),
                    )
                    .await
                    .expect("response");
                to_bytes(response.into_body(), 1024 * 1024)
                    .await
                    .expect("consume body");
            }
        }
        // Trigger one failure after the HTTP handler has returned, proving the SSE body retains its own context.
        let subscription_id = Uuid::new_v4();
        Jobs::submit(&db, subscription_id, &hash, None, None)
            .await
            .expect("subscription job");
        let response = app
            .oneshot(
                Request::get(format!("/api/jobs/events?id={subscription_id}"))
                    .body(Body::empty())
                    .expect("SSE request"),
            )
            .await
            .expect("SSE response");
        let mut stream = response.into_body().into_data_stream();
        stream.next().await.expect("first snapshot").expect("frame");
        parse_jobs::Entity::delete_by_id(subscription_id)
            .exec(&db)
            .await
            .expect("remove observed job");
        tokio::time::timeout(Duration::from_secs(5), stream.next().with_subscriber(tracing::subscriber::NoSubscriber::default()))
            .await
            .expect("error timeout")
            .expect("error event")
            .expect("frame");
        assert!(stream.next().await.is_none());
        // Check after terminal transitions so a failing assertion cannot leave a queued fixture for another test.
        let logs = std::fs::read_to_string(log.path()).expect("captured logs");
        for id in [first_id, second_id] {
            for marker in [
                "accepted PDF job",
                "starting document parse",
                "probe-layout-async",
                "probe-layout-cpu",
                "probe-layout-session",
                "completed document parse",
            ] {
                let lines: Vec<_> = logs
                    .lines()
                    .filter(|line| {
                        line.contains(marker)
                            && line.contains(&format!("job_id={id}"))
                    })
                    .collect();
                assert!(
                    !lines.is_empty(),
                    "missing correlated {marker} for {id}:\n{logs}"
                );
                for line in lines {
                    assert!(
                        line.contains(&format!("pdf_hash={hash}")),
                        "missing hash: {line}"
                    );
                    for other in ids.into_iter().filter(|other| *other != id) {
                        assert!(
                            !line.contains(&format!("job_id={other}")),
                            "mixed jobs: {line}"
                        );
                    }
                }
            }
        }
        let invalid_hash = blake3::hash(invalid).to_hex().to_string();
        for attempt in 1..=2 {
            let line = logs
                .lines()
                .find(|line| {
                    line.contains("failed to open PDF:")
                        && line.contains(&format!("job_id={invalid_id}"))
                        && line.contains(&format!("attempt={attempt}"))
                })
                .expect("PDFium thread retains retry context");
            assert!(line.contains(&format!("pdf_hash={invalid_hash}")));
        }
        assert!(logs.lines().any(|line| line.contains("http_request")
            && line.contains("GET /api/jobs/")
            && line.contains(&format!("job_id={first_id}"))
            && line.contains("request_id=")));
        assert!(
            logs.lines()
                .any(|line| line.contains("SSE subscription failed")
                    && line.contains(&format!("job_id={subscription_id}"))
                    && line.contains(&format!("pdf_hash={hash}"))
                    && line.contains("request_id="))
        );
    }.with_subscriber(subscriber).await;
}
