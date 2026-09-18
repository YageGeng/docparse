use docparse_common::timing::Timings;
use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_core::{
    LocalPdfiumProvider, ParseObserver, ParseProgress, PdfInput, PdfiumProvider,
};
use docparse_server::pdfium_pool::PdfiumPool;

use std::{path::Path, sync::Arc, time::Duration};

#[path = "../../core/tests/common/pdf.rs"]
mod pdf;

static PROCESS_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// Expired idle tokens must not evict another process's healthy reservation during crash recovery.
#[tokio::test]
async fn stale_idle_token_does_not_restart_a_healthy_reservation() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    for hold_first in [true, false] {
        let pool = PdfiumPool::start(
            2,
            Path::new(env!("CARGO_BIN_EXE_docparse-server")),
        )
        .await
        .expect("pool");
        let first = pool.reserve().await.expect("first reservation");
        let second = pool.reserve().await.expect("second reservation");
        let held = if hold_first {
            drop(second);
            first
        } else {
            drop(first);
            second
        };
        let original = worker_pids();
        let victim = *original.first().expect("worker");
        let healthy = *original.get(1).expect("peer worker");
        let killed = std::process::Command::new("kill")
            .args(["-KILL", &victim.to_string()])
            .status()
            .expect("kill owned worker");
        let replacement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pids = worker_pids();
                if pids.len() == 2 && !pids.contains(&victim) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        // Let the replacement finish its IPC handshake without consuming the stale idle token.
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(held);
        tokio::time::sleep(Duration::from_millis(350)).await;
        let after_return = worker_pids();
        let reused =
            tokio::time::timeout(Duration::from_secs(5), pool.reserve()).await;
        let admitted = matches!(reused, Ok(Ok(_)));
        drop(reused);
        let cleanup = pool.shutdown().await;
        cleanup.expect("pool cleanup");
        assert!(killed.success());
        replacement.expect("replacement startup");
        assert!(admitted, "stale tokens must not prevent future admission");
        assert!(
            after_return.contains(&healthy),
            "returning a healthy reservation restarted PID {healthy}: {after_return:?}; hold_first={hold_first}"
        );
    }
}

/// Keeps downstream processing open after the only PDFium process has completed rendering.
struct WaitingLayout {
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl docparse_layout::LayoutEngine for WaitingLayout {
    /// Identifies the deterministic downstream scheduling boundary.
    fn name(&self) -> &str {
        "waiting-layout"
    }
    /// Supplies a stable revision independently of model artifacts.
    fn model_revision(&self) -> &str {
        "1"
    }
    /// Waits for test-controlled downstream capacity while the real PDFium lease is independently released.
    fn detect(
        &self,
        _request: docparse_layout::LayoutRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<
            Vec<docparse_layout::LayoutDetection>,
            docparse_layout::LayoutError,
        >,
    > {
        Box::pin(async {
            self.entered.notify_one();
            self.release.acquire().await.expect("gate").forget();
            Ok(Vec::new())
        })
    }
}

/// Observes completed real Render operations, including results not yet consumed by a model.
#[derive(Default)]
struct RenderCount(std::sync::atomic::AtomicUsize);
impl ParseObserver for RenderCount {
    /// Progress events do not affect raster accounting.
    fn on_progress(&self, _progress: ParseProgress) {}
    /// Counts real render completions without consulting the queue's own accounting.
    fn on_timing(&self, timing: docparse_core::Timing) {
        if timing.stage == docparse_common::timing::TimingStage::PdfRender {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Idle processes admit the next PDF before old model work finishes, while the shared render queue blocks its rasterization.
#[tokio::test]
async fn idle_reservation_admits_pdf_while_render_queue_is_full() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let pool =
        PdfiumPool::start(1, Path::new(env!("CARGO_BIN_EXE_docparse-server")))
            .await
            .expect("pool");
    let pids = worker_pids();
    for _ in 0..3 {
        drop(pool.reserve().await.expect("unused reservation"));
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let mut raw = docparse_config::RawConfig::default();
    raw.render.queue_size = 1;
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.tsr.mode = docparse_config::TableMode::RulesOnly;
    let layout = Arc::new(WaitingLayout {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    let parser = docparse_core::DocParser::builder()
        .config(Arc::new(
            docparse_config::ValidatedConfig::try_from(raw).expect("config"),
        ))
        .layout_engine(
            Arc::clone(&layout) as Arc<dyn docparse_layout::LayoutEngine>
        )
        .build()
        .await
        .expect("parser");
    let observer = Arc::new(RenderCount::default());
    let first = pool
        .reserve()
        .await
        .expect("first reservation")
        .open(PdfInput::Path(fixture.clone()), Timings::default())
        .await
        .expect("first open");
    let first_parser = parser.clone();
    let first_observer = Arc::clone(&observer);
    let first_task = tokio::spawn(async move {
        first_parser
            .parse_session_with_options(
                first,
                docparse_core::ParseOptions::builder()
                    .observer(Some(first_observer.as_ref()))
                    .build(),
            )
            .await
    });
    let entered = tokio::time::timeout(
        Duration::from_secs(10),
        layout.entered.notified(),
    )
    .await;
    if entered.is_err() {
        first_task.abort();
        pool.shutdown().await.expect("cleanup");
    }
    assert!(
        entered.is_ok(),
        "first page must use its already-open session without a second acquire"
    );
    let next =
        tokio::time::timeout(Duration::from_secs(5), pool.reserve()).await;
    if next.is_err() {
        layout.release.add_permits(2);
        first_task.await.expect("join").expect("first document");
        pool.shutdown().await.expect("cleanup");
        assert!(
            next.is_ok(),
            "idle worker must admit a new PDF before the old task finishes"
        );
        return;
    }
    let second = next
        .expect("reservation deadline")
        .expect("reservation")
        .open(PdfInput::Path(fixture), Timings::default())
        .await
        .expect("second open");
    let still_running = !first_task.is_finished();
    let second_observer = Arc::clone(&observer);
    let second_task = tokio::spawn(async move {
        parser
            .parse_session_with_options(
                second,
                docparse_core::ParseOptions::builder()
                    .observer(Some(second_observer.as_ref()))
                    .build(),
            )
            .await
    });
    let prematurely_started = tokio::time::timeout(
        Duration::from_millis(100),
        layout.entered.notified(),
    )
    .await;
    let renders = observer.0.load(std::sync::atomic::Ordering::SeqCst);
    layout.release.add_permits(2);
    first_task.await.expect("first join").expect("first result");
    second_task
        .await
        .expect("second join")
        .expect("second result");
    let reused = worker_pids();
    pool.shutdown().await.expect("shutdown");
    assert!(still_running);
    assert!(prematurely_started.is_err());
    assert_eq!(
        renders, 1,
        "the next PDF may open, but cannot render while the old delivery owns the only slot"
    );
    assert_eq!(
        pids, reused,
        "unused reservations and ordinary closes must not restart PDFium"
    );
    assert_eq!(observer.0.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Reads actual child PIDs instead of trusting the pool's own accounting.
fn worker_pids() -> Vec<u32> {
    let output = std::process::Command::new("ps")
        .args(["-ax", "-o", "ppid=,pid=,args="])
        .output()
        .expect("process inventory");
    String::from_utf8(output.stdout)
        .expect("process inventory text")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let parent = fields.next()?.parse::<u32>().ok()?;
            let pid = fields.next()?.parse::<u32>().ok()?;
            (parent == std::process::id()
                && fields.next()?.ends_with("docparse-pdfium-worker"))
            .then_some(pid)
        })
        .collect()
}

/// No inference may run while the real worker is stopped before its first raster.
struct UnreachableLayout;

impl docparse_layout::LayoutEngine for UnreachableLayout {
    /// Identifies the deliberately unused inference dependency.
    fn name(&self) -> &str {
        "unreachable-layout"
    }
    /// Provides a stable revision without loading model files.
    fn model_revision(&self) -> &str {
        "test"
    }
    /// Fails if the cancellation test unexpectedly reaches inference.
    #[allow(clippy::panic)] // Inference is intentionally unreachable in this cancellation test.
    fn detect(
        &self,
        _request: docparse_layout::LayoutRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<
            Vec<docparse_layout::LayoutDetection>,
            docparse_layout::LayoutError,
        >,
    > {
        Box::pin(async { panic!("stopped PDFium must not produce a raster") })
    }
}

/// Stops the owned worker after extraction and wakes the cancelling caller.
struct StopBeforeRender {
    pid: u32,
    stopped: Arc<tokio::sync::Notify>,
}

impl ParseObserver for StopBeforeRender {
    /// Freezes only this test's child before the parser spawns its render producer.
    fn on_progress(&self, progress: ParseProgress) {
        if matches!(progress, ParseProgress::Analyzing { completed: 0, .. }) {
            assert!(
                std::process::Command::new("kill")
                    .args(["-STOP", &self.pid.to_string()])
                    .status()
                    .expect("stop owned worker")
                    .success()
            );
            self.stopped.notify_one();
        }
    }
}

/// Cancelling a full parse must retire its stuck renderer and admit another document.
#[tokio::test]
async fn cancelled_parser_releases_hung_renderer() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let mut raw = docparse_config::RawConfig::default();
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.ocr.policy = docparse_config::OcrPolicy::Disabled;
    raw.tsr.mode = docparse_config::TableMode::RulesOnly;
    let pool =
        PdfiumPool::start(1, Path::new(env!("CARGO_BIN_EXE_docparse-server")))
            .await
            .expect("one worker");
    let parser = docparse_core::DocParser::builder()
        .config(Arc::new(
            docparse_config::ValidatedConfig::try_from(raw).expect("config"),
        ))
        .layout_engine(Arc::new(UnreachableLayout))
        .pdfium_provider(Arc::clone(&pool) as Arc<dyn PdfiumProvider>)
        .build()
        .await
        .expect("parser");
    let pid = *worker_pids().first().expect("worker PID");
    let stopped = Arc::new(tokio::sync::Notify::new());
    let observer = StopBeforeRender {
        pid,
        stopped: Arc::clone(&stopped),
    };
    let input = fixture.clone();
    let parse = tokio::spawn(async move {
        parser
            .parse_path_with_options(
                input,
                docparse_core::ParseOptions::builder()
                    .observer(Some(&observer))
                    .build(),
            )
            .await
    });
    let reached =
        tokio::time::timeout(Duration::from_secs(10), stopped.notified()).await;
    parse.abort();
    let cancelled = parse.await;
    let recovered = tokio::time::timeout(
        Duration::from_secs(10),
        pool.open(
            PdfInput::Path(fixture),
            &RuntimeConfig::default(),
            Timings::default(),
        ),
    )
    .await;
    let after_cancel = worker_pids();
    let capacity_recovered = matches!(recovered, Ok(Ok(_)));
    if let Ok(Ok(session)) = recovered {
        session.close().await.expect("close recovered document");
    }
    // Clean up the stopped child even when the regression assertion fails.
    let cleanup = pool.shutdown().await;
    assert!(worker_pids().is_empty());
    cleanup.expect("stop pool");
    reached.expect("parser reached rendering");
    assert!(cancelled.expect_err("cancelled parse").is_cancelled());
    assert!(
        capacity_recovered,
        "cancelled parser retained its render process slot"
    );
    assert!(
        !after_cancel.contains(&pid),
        "stuck worker must be reaped before replacement"
    );
}

/// Cancellation preserves crash history; only a successfully closed document resets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_preserves_crash_budget_until_successful_close() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let limits = RuntimeConfig::default();
    for complete_document in [false, true] {
        let pool = PdfiumPool::start(
            1,
            Path::new(env!("CARGO_BIN_EXE_docparse-server")),
        )
        .await
        .expect("pool");
        let first = pool
            .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
            .await
            .expect("first lease");
        let pid = *worker_pids().first().expect("worker PID");
        assert!(
            std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status()
                .expect("kill worker")
                .success()
        );
        first.pre_scan_page(1, None).await.expect_err("first crash");
        drop(first);
        let replacement = tokio::time::timeout(
            Duration::from_secs(10),
            pool.open(
                PdfInput::Path(fixture.clone()),
                &limits,
                Timings::default(),
            ),
        )
        .await
        .expect("replacement deadline")
        .expect("one replacement");
        drop(replacement);
        let mut next = tokio::time::timeout(
            Duration::from_secs(10),
            pool.open(
                PdfInput::Path(fixture.clone()),
                &limits,
                Timings::default(),
            ),
        )
        .await
        .expect("cancel cleanup deadline")
        .expect("replacement after cancellation");
        if complete_document {
            next.close().await.expect("successful document");
            next = pool
                .open(
                    PdfInput::Path(fixture.clone()),
                    &limits,
                    Timings::default(),
                )
                .await
                .expect("reused worker");
        }
        let pid = *worker_pids().first().expect("worker PID");
        assert!(
            std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status()
                .expect("kill replacement")
                .success()
        );
        next.pre_scan_page(1, None).await.expect_err("second crash");
        drop(next);
        let admitted = tokio::time::timeout(
            Duration::from_secs(10),
            pool.open(
                PdfInput::Path(fixture.clone()),
                &limits,
                Timings::default(),
            ),
        )
        .await;
        let allowed = matches!(admitted, Ok(Ok(_)));
        drop(admitted);
        let cleanup = pool.shutdown().await;
        assert!(worker_pids().is_empty());
        assert_eq!(
            allowed, complete_document,
            "only successful Close may reset crash history"
        );
        assert_eq!(cleanup.is_ok(), complete_document);
    }
}

/// Shutdown must be acknowledged both while idle and with an open document, followed by exit code zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_shutdown_releases_document_and_exits_successfully() {
    use docparse_core::pdfium_ipc::{
        Bootstrap, Command, Hello, Outcome, Reply, Request, Source,
        WORKER_BINARY,
    };
    use ipc_channel::ipc::{self, IpcSender};
    use tokio::io::{AsyncBufReadExt, BufReader};
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let binary = Path::new(env!("CARGO_BIN_EXE_docparse-server"))
        .parent()
        .expect("binary directory")
        .join(WORKER_BINARY);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    for open_document in [false, true] {
        let directory = tempfile::tempdir().expect("bootstrap directory");
        let mut child = tokio::process::Command::new(&binary)
            .stdout(std::process::Stdio::piped())
            .env("TMPDIR", directory.path())
            .kill_on_drop(true)
            .spawn()
            .expect("worker");
        let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            reader.read_line(&mut line),
        )
        .await
        .expect("bootstrap deadline")
        .expect("bootstrap");
        let hello: Hello = serde_json::from_str(&line).expect("hello");
        hello.validate().expect("compatible worker");
        let fixture = fixture.clone();
        let exchange =
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let (commands, receiver) =
                    ipc::channel::<Request>().map_err(|e| e.to_string())?;
                let (responses, replies) =
                    ipc::channel::<Reply>().map_err(|e| e.to_string())?;
                let connector = IpcSender::<Bootstrap>::connect(hello.endpoint)
                    .map_err(|e| e.to_string())?;
                connector
                    .send(Bootstrap {
                        commands: receiver,
                        responses,
                    })
                    .map_err(|e| e.to_string())?;
                let ready = replies
                    .try_recv_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?;
                assert!(matches!(ready.outcome, Outcome::Ready));
                if open_document {
                    commands
                        .send(Request {
                            lease: 1,
                            id: 1,
                            command: Command::Open(Source::Path(fixture)),
                        })
                        .map_err(|e| e.to_string())?;
                    let opened = replies
                        .try_recv_timeout(Duration::from_secs(5))
                        .map_err(|e| e.to_string())?;
                    assert!(matches!(opened.outcome, Outcome::Opened(1)));
                }
                commands
                    .send(Request {
                        lease: 0,
                        id: 2,
                        command: Command::Shutdown,
                    })
                    .map_err(|e| e.to_string())?;
                let reply = replies
                    .try_recv_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?;
                assert_eq!((reply.lease, reply.id), (0, 2));
                assert!(matches!(reply.outcome, Outcome::Closed));
                Ok(())
            });
        let result = exchange.await.expect("bridge task");
        let status =
            tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        // Reap even a worker that failed to honor the graceful shutdown request.
        if status.is_err() {
            child.kill().await.expect("force worker cleanup");
        }
        result.expect("graceful shutdown acknowledgement");
        assert!(
            status
                .expect("exit deadline")
                .expect("child exit")
                .success()
        );
    }
}

/// A lease holds one process until Close; separate processes must open documents concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_processes_enforce_capacity_preserve_output_and_recover_after_cancel()
 {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let binary = Path::new(env!("CARGO_BIN_EXE_docparse-server"));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let limits = RuntimeConfig::default();
    let pool = PdfiumPool::start(1, binary).await.expect("start pool");
    let first = pool
        .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
        .await
        .expect("first lease");
    let pending =
        pool.open(PdfInput::Path(fixture.clone()), &limits, Timings::default());
    tokio::pin!(pending);
    tokio::time::timeout(Duration::from_millis(150), &mut pending)
        .await
        .map(|_| ())
        .expect_err("second document must wait for the occupied slot");
    assert_eq!(worker_pids().len(), 1);
    let pid = *worker_pids().first().expect("worker PID");
    let group = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pgid="])
        .output()
        .expect("worker process group");
    let group = String::from_utf8(group.stdout)
        .expect("process group text")
        .trim()
        .parse::<u32>()
        .expect("process group ID");
    first.close().await.expect("release first lease");
    let second = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("capacity released")
        .expect("second lease");
    drop(second);
    let recovered = tokio::time::timeout(
        Duration::from_secs(10),
        pool.open(PdfInput::Path(fixture.clone()), &limits, Timings::default()),
    )
    .await
    .expect("cancel cleanup")
    .expect("replacement lease");
    recovered.close().await.expect("close replacement");
    // Closing must never race its successful acknowledgement against lease cancellation.
    for _ in 0..20 {
        pool.open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
            .await
            .expect("reused worker")
            .close()
            .await
            .expect("Close acknowledgement");
    }
    pool.shutdown().await.expect("stop first pool");
    assert!(worker_pids().is_empty());
    assert_eq!(
        group, pid,
        "terminal interrupts must reach the supervisor instead of killing its workers"
    );

    let pool = PdfiumPool::start(2, binary)
        .await
        .expect("start two workers");
    let a = pool
        .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
        .await
        .expect("document a");
    let bytes = std::sync::Arc::<[u8]>::from(
        std::fs::read(&fixture).expect("PDF bytes"),
    );
    let b = tokio::time::timeout(
        Duration::from_secs(5),
        pool.open(PdfInput::Bytes(bytes), &limits, Timings::default()),
    )
    .await
    .expect("parallel PDFium opening")
    .expect("document b");
    assert_eq!(worker_pids().len(), 2);
    let local = LocalPdfiumProvider;
    let reference = local
        .open(PdfInput::Path(fixture), &limits, Timings::default())
        .await
        .expect("local reference");
    let expected = reference.pre_scan_page(1, None).await.expect("local scan");
    for session in [&*a, &*b] {
        let actual = session.pre_scan_page(1, None).await.expect("remote scan");
        assert_eq!(actual.extracted, expected.extracted);
        let raster = session
            .render_page(1, &RenderConfig::default())
            .await
            .expect("remote render");
        let expected_raster = reference
            .render_page(1, &RenderConfig::default())
            .await
            .expect("local render");
        assert_eq!(raster.transform, expected_raster.transform);
        assert_eq!(raster.image.data(), expected_raster.image.data());
    }
    reference.close().await.expect("close local");
    a.close().await.expect("close a");
    b.close().await.expect("close b");
    pool.shutdown().await.expect("stop pool");
    pool.shutdown().await.expect("idempotent stop");
    assert!(worker_pids().is_empty());
}

/// Invalid budgets and missing package artifacts must fail without panic or leaked children.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_budgets_and_missing_worker_fail_before_admission() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let binary = Path::new(env!("CARGO_BIN_EXE_docparse-server"));
    PdfiumPool::start(0, binary)
        .await
        .map(|_| ())
        .expect_err("invalid pool startup must fail");
    PdfiumPool::start(usize::MAX, binary)
        .await
        .map(|_| ())
        .expect_err("invalid pool startup must fail");
    let directory = tempfile::tempdir().expect("temporary package");
    PdfiumPool::start(2, &directory.path().join("docparse-server"))
        .await
        .map(|_| ())
        .expect_err("invalid pool startup must fail");
    assert!(worker_pids().is_empty());
}

/// Repeated worker crashes must stop admission rather than spawn an unbounded replacement loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashed_workers_are_reaped_and_repeat_failure_closes_pool() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let binary = Path::new(env!("CARGO_BIN_EXE_docparse-server"));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let limits = RuntimeConfig::default();
    let pool = PdfiumPool::start(1, binary).await.expect("pool");
    let session = pool
        .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
        .await
        .expect("lease");
    let first = *worker_pids().first().expect("live worker PID");
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &first.to_string()])
            .status()
            .expect("kill owned worker")
            .success()
    );
    session
        .pre_scan_page(1, None)
        .await
        .expect_err("dead worker must fail");
    drop(session);
    let replacement = tokio::time::timeout(
        Duration::from_secs(10),
        pool.open(PdfInput::Path(fixture), &limits, Timings::default()),
    )
    .await
    .expect("replacement deadline")
    .expect("replacement");
    let second = *worker_pids().first().expect("live worker PID");
    assert_ne!(first, second);
    assert_eq!(worker_pids().len(), 1);
    assert!(
        std::process::Command::new("kill")
            .args(["-KILL", &second.to_string()])
            .status()
            .expect("kill replacement")
            .success()
    );
    replacement
        .pre_scan_page(1, None)
        .await
        .expect_err("dead replacement must fail");
    drop(replacement);
    tokio::time::timeout(Duration::from_secs(10), pool.stopped())
        .await
        .expect("pool failure notification");
    pool.shutdown()
        .await
        .expect_err("repeated worker crashes must fail the pool");
    assert!(worker_pids().is_empty());
}

/// Real model inference must produce the same canonical document with local and remote PDFium.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the installed layout model"]
async fn real_layout_parser_preserves_canonical_results() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut raw =
        docparse_config::ConfigLoader::new(root.join("docparse.toml"))
            .load_raw()
            .expect("configuration");
    raw.ocr.policy = docparse_config::OcrPolicy::Disabled;
    raw.tsr.mode = docparse_config::TableMode::RulesOnly;
    let config = std::sync::Arc::new(
        docparse_config::ValidatedConfig::try_from(raw)
            .expect("parser configuration"),
    );
    let engine = std::sync::Arc::new(
        docparse_layout::PpDocLayoutV3Engine::from_config(
            std::sync::Arc::clone(&config),
        )
        .await
        .expect("real layout engine"),
    );
    let local = docparse_core::DocParser::builder()
        .config(std::sync::Arc::clone(&config))
        .layout_engine(std::sync::Arc::clone(&engine)
            as std::sync::Arc<dyn docparse_layout::LayoutEngine>)
        .build()
        .await
        .expect("local parser");
    let pool =
        PdfiumPool::start(2, Path::new(env!("CARGO_BIN_EXE_docparse-server")))
            .await
            .expect("pool");
    let remote = docparse_core::DocParser::builder()
        .config(config)
        .layout_engine(engine)
        .pdfium_provider(
            std::sync::Arc::clone(&pool) as std::sync::Arc<dyn PdfiumProvider>
        )
        .build()
        .await
        .expect("remote parser");
    for name in [
        "table_layout.pdf",
        "embedded_cjk_90.pdf",
        "glyph_recovery.pdf",
    ] {
        let pdf = root.join("crates/core/tests/fixtures/pdf").join(name);
        let expected = local.parse_path(&pdf).await.expect("local parse");
        let actual = remote.parse_path(&pdf).await.expect("remote parse");
        assert_eq!(
            serde_json::to_value(actual).expect("remote JSON"),
            serde_json::to_value(expected).expect("local JSON"),
            "{name}"
        );
    }
    pool.shutdown().await.expect("close real-model pool");
    assert!(worker_pids().is_empty());
}

/// A bad protocol version must never become an idle pool slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incompatible_worker_handshake_is_rejected_and_reaped() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let package = tempfile::tempdir().expect("temporary package");
    let worker = package.path().join("docparse-pdfium-worker");
    std::fs::write(&worker, "#!/bin/sh\nprintf '%s\\n' '{\"protocol\":999,\"version\":\"invalid\",\"endpoint\":\"unused\"}'\n").expect("invalid protocol fixture");
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700))
        .expect("executable fixture");
    PdfiumPool::start(2, &package.path().join("docparse-server"))
        .await
        .map(|_| ())
        .expect_err("invalid pool startup must fail");
    assert!(worker_pids().is_empty());
}

/// Records actual outline callbacks and can exercise the parent's panic boundary.
struct Outlines {
    calls: std::sync::atomic::AtomicUsize,
    panics: bool,
}
impl docparse_core::GlyphResolver for Outlines {
    /// Returns a recognizable value only for real outlines supplied across the IPC boundary.
    fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(!self.panics, "intentional resolver failure");
        (!segments.is_empty()).then(|| "✓".to_owned())
    }
}

/// Reverse IPC must preserve custom outline recovery and retire a worker when its resolver fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn glyph_callbacks_preserve_recovery_and_failures_are_not_silenced() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/glyph_recovery.pdf");
    let limits = RuntimeConfig::default();
    let resolver = Arc::new(Outlines {
        calls: AtomicUsize::new(0),
        panics: false,
    });
    let local = LocalPdfiumProvider;
    let reference = local
        .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
        .await
        .expect("local glyph fixture");
    let expected =
        reference
            .pre_scan_page(
                1,
                Some(Arc::clone(&resolver)
                    as Arc<dyn docparse_core::GlyphResolver>),
            )
            .await
            .expect("local outlines");
    let local_calls = resolver.calls.load(Ordering::Relaxed);
    assert!(local_calls > 0, "fixture must exercise outline recovery");
    reference.close().await.expect("close local outlines");
    let pool =
        PdfiumPool::start(1, Path::new(env!("CARGO_BIN_EXE_docparse-server")))
            .await
            .expect("pool");
    let remote = pool
        .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
        .await
        .expect("remote glyph fixture");
    let actual =
        remote
            .pre_scan_page(
                1,
                Some(Arc::clone(&resolver)
                    as Arc<dyn docparse_core::GlyphResolver>),
            )
            .await
            .expect("reverse IPC glyph lookup");
    assert_eq!(actual.extracted, expected.extracted);
    assert!(resolver.calls.load(Ordering::Relaxed) > local_calls);
    remote.close().await.expect("close remote outlines");
    let failing = pool
        .open(PdfInput::Path(fixture), &limits, Timings::default())
        .await
        .expect("failing resolver lease");
    failing
        .pre_scan_page(
            1,
            Some(Arc::new(Outlines {
                calls: AtomicUsize::new(0),
                panics: true,
            })),
        )
        .await
        .expect_err("resolver failure must not become missing text");
    drop(failing);
    pool.shutdown().await.expect("stop glyph pool");
    assert!(worker_pids().is_empty());
}

/// Startup cancellation and explicit shutdown must clean up even while callers still hold leases.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_startup_and_busy_shutdown_release_owned_processes() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let binary = Path::new(env!("CARGO_BIN_EXE_docparse-server"));
    let startup =
        tokio::spawn(async move { PdfiumPool::start(4, binary).await });
    tokio::task::yield_now().await;
    startup.abort();
    if let Ok(Ok(pool)) = startup.await {
        pool.shutdown().await.expect("completed startup cleanup");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while !worker_pids().is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("cancelled startup must reap workers");

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    let limits = RuntimeConfig::default();
    let pool = PdfiumPool::start(4, binary).await.expect("four workers");
    let mut sessions = Vec::new();
    for _ in 0..4 {
        sessions.push(
            pool.open(
                PdfInput::Path(fixture.clone()),
                &limits,
                Timings::default(),
            )
            .await
            .expect("document lease"),
        );
    }
    assert_eq!(worker_pids().len(), 4);
    tokio::time::timeout(
        Duration::from_millis(100),
        pool.open(PdfInput::Path(fixture.clone()), &limits, Timings::default()),
    )
    .await
    .map(|_| ())
    .expect_err("fifth document must wait");
    pool.shutdown().await.expect("busy shutdown");
    assert!(worker_pids().is_empty());
    for session in sessions {
        session
            .pre_scan_page(1, None)
            .await
            .expect_err("shutdown invalidates held leases");
    }
    pool.open(PdfInput::Path(fixture), &limits, Timings::default())
        .await
        .map(|_| ())
        .expect_err("shutdown rejects new documents");
}

/// A bounded blocking callback makes cancellation observable while a real worker is mid-scan.
struct BlockingOutlines {
    entered: std::sync::atomic::AtomicBool,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl docparse_core::GlyphResolver for BlockingOutlines {
    /// Blocks only the first real outline, allowing the test to control an in-flight IPC request.
    fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
        if !self.entered.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let _ = self
                .release
                .lock()
                .expect("callback release")
                .recv_timeout(Duration::from_secs(5));
        }
        (!segments.is_empty()).then(|| "✓".to_owned())
    }
}

/// Cancelling one admitted operation must retire its uncertain worker even if the session remains alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_an_operation_retires_its_worker_without_dropping_the_session()
 {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let binary = Path::new(env!("CARGO_BIN_EXE_docparse-server"));
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/glyph_recovery.pdf");
    let limits = RuntimeConfig::default();
    let pool = PdfiumPool::start(1, binary).await.expect("pool");
    let session = pool
        .open(PdfInput::Path(fixture), &limits, Timings::default())
        .await
        .expect("glyph lease");
    let pid = *worker_pids().first().expect("worker");
    let (release, released) = std::sync::mpsc::channel();
    let resolver = Arc::new(BlockingOutlines {
        entered: AtomicBool::new(false),
        release: std::sync::Mutex::new(released),
    });
    {
        let scan =
            session.pre_scan_page(
                1,
                Some(Arc::clone(&resolver)
                    as Arc<dyn docparse_core::GlyphResolver>),
            );
        tokio::pin!(scan);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut scan => { result.expect("scan cannot fail before callback"); unreachable!("callback must block"); }
                _ = async { while !resolver.entered.load(Ordering::SeqCst) { tokio::time::sleep(Duration::from_millis(1)).await; } } => {}
            }
        }).await.expect("callback entered");
        tokio::time::timeout(
            Duration::from_millis(50),
            session.render_page(1, &RenderConfig::default()),
        )
        .await
        .map(|_| ())
        .expect_err("another operation waits behind the scan");
        assert!(
            worker_pids().contains(&pid),
            "cancelling a queued operation must not cancel the admitted scan"
        );
    }
    // Cancellation allows the admitted callback to finish before the queued Shutdown is handled.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        worker_pids().contains(&pid),
        "graceful cancellation must allow the active call to finish"
    );
    release.send(()).expect("release parent callback");
    tokio::time::timeout(Duration::from_secs(2), async {
        while worker_pids().contains(&pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping the scan future must retire the worker");
    session
        .pre_scan_page(1, None)
        .await
        .expect_err("uncertain session cannot be reused");
    drop(session);
    pool.shutdown()
        .await
        .expect("shutdown after cancelled operation");
    assert!(worker_pids().is_empty());
}

/// Invalid PDFs and deliberate cancellation are not repeated worker crashes and must not stop admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_documents_and_repeated_cancellation_do_not_poison_the_pool() {
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let limits = RuntimeConfig::default();
    let pool =
        PdfiumPool::start(1, Path::new(env!("CARGO_BIN_EXE_docparse-server")))
            .await
            .expect("pool");
    let initial_pid = *worker_pids().first().expect("worker PID");
    let empty = pdf::document(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [] /Count 0 >>".to_owned(),
    ]);
    for bytes in [b"%PDF-invalid".to_vec(), empty] {
        for _ in 0..3 {
            pool.open(
                PdfInput::Bytes(std::sync::Arc::from(bytes.clone())),
                &limits,
                Timings::default(),
            )
            .await
            .map(|_| ())
            .expect_err("invalid document");
        }
    }
    assert_eq!(
        worker_pids(),
        vec![initial_pid],
        "document rejection must reuse the healthy worker"
    );
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/pdf/extraction_metadata.pdf");
    for _ in 0..3 {
        let session = pool
            .open(PdfInput::Path(fixture.clone()), &limits, Timings::default())
            .await
            .expect("cancellation must not poison the pool");
        drop(session);
    }
    let session = pool
        .open(PdfInput::Path(fixture), &limits, Timings::default())
        .await
        .expect("healthy admission after cancellation");
    session.close().await.expect("close");
    pool.shutdown().await.expect("shutdown");
    assert!(worker_pids().is_empty());
}

/// The parent must own bootstrap temporary files so a failed child cannot leave Unix rendezvous files behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_bootstrap_cleans_worker_temporary_files() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = PROCESS_TEST_LOCK.lock().await;
    let package = tempfile::tempdir().expect("temporary package");
    let observed = package.path().join("observed-tempdir");
    let marker = format!("docparse-ipc-probe-{}", std::process::id());
    let worker = package.path().join("docparse-pdfium-worker");
    let script = format!(
        "#!/bin/sh\nprintf '%s' \"$TMPDIR\" > '{}'\nprintf 'owned' > \"$TMPDIR/{}\"\nprintf '%s\\n' '{{\"protocol\":999,\"version\":\"invalid\",\"endpoint\":\"unused\"}}'\n",
        observed.display(),
        marker
    );
    std::fs::write(&worker, script).expect("bootstrap fixture");
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700))
        .expect("executable");
    PdfiumPool::start(1, &package.path().join("docparse-server"))
        .await
        .map(|_| ())
        .expect_err("invalid handshake");
    let directory = std::path::PathBuf::from(
        std::fs::read_to_string(observed).expect("observed worker tempdir"),
    );
    let leftover = directory.join(marker);
    let leaked = leftover.exists();
    if leaked {
        std::fs::remove_file(leftover)
            .expect("remove only the test-created marker");
    }
    assert!(
        !leaked,
        "the parent must clean files created before worker readiness"
    );
}
