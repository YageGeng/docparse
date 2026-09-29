use docparse_common::{SessionWorker, TaskError, run_cpu};
use std::{sync::Arc, time::Duration};

/// A running CPU job must not consume the caller's only blocking thread.
#[test]
fn cpu_work_leaves_the_caller_blocking_pool_available() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let cpu = tokio::spawn(run_cpu(move || {
            started.send(()).expect("started");
            blocked
                .recv_timeout(Duration::from_secs(5))
                .expect("release");
        }));
        entered.await.expect("CPU job entered");
        let available = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::task::spawn_blocking(|| 42),
        )
        .await;
        release.send(()).expect("release CPU job");
        cpu.await.expect("caller").expect("CPU completion");
        assert_eq!(
            available
                .expect("caller blocking pool remains available")
                .expect("blocking work"),
            42
        );
    });
}

/// Native inference waits must not occupy the caller's blocking pool either.
#[test]
fn model_wait_leaves_the_caller_blocking_pool_available() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let worker = Arc::new(
            SessionWorker::new(|| Ok::<_, TaskError>(()))
                .await
                .expect("session"),
        );
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let caller = Arc::clone(&worker);
        let model = tokio::spawn(async move {
            caller
                .run(move |_| {
                    started.send(()).expect("started");
                    blocked
                        .recv_timeout(Duration::from_secs(5))
                        .expect("release");
                })
                .await
        });
        entered.await.expect("model entered");
        let available = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::task::spawn_blocking(|| 42),
        )
        .await;
        release.send(()).expect("release model");
        model.await.expect("caller").expect("model completion");
        assert_eq!(
            available
                .expect("caller blocking pool remains available")
                .expect("blocking work"),
            42
        );
    });
}

/// Process shutdown must drain native CPU work even when its original caller was canceled.
#[tokio::test]
async fn drain_waits_for_cancelled_cpu_work() {
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let task = tokio::spawn(run_cpu(move || {
        started.send(()).expect("started");
        blocked
            .recv_timeout(Duration::from_secs(5))
            .expect("release");
    }));
    entered.await.expect("CPU work entered");
    task.abort();
    task.await.expect_err("canceled caller");
    let premature = tokio::time::timeout(
        Duration::from_millis(30),
        docparse_common::drain_cpu(),
    )
    .await;
    release.send(()).expect("release native work");
    assert!(
        premature.is_err(),
        "drain cannot outlive unfinished CPU work"
    );
    tokio::time::timeout(Duration::from_secs(1), docparse_common::drain_cpu())
        .await
        .expect("drain completed")
        .expect("CPU pool");
}

/// A discarded result retains real resources until its destructor has finished.
#[derive(Debug)]
struct DroppingOutput {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
    finished: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for DroppingOutput {
    /// Holds resource destruction at a barrier independently of the computation that produced it.
    fn drop(&mut self) {
        self.entered
            .take()
            .expect("drop notification")
            .send(())
            .expect("observer");
        self.release
            .recv_timeout(Duration::from_secs(5))
            .expect("release destruction");
        self.finished
            .take()
            .expect("completion notification")
            .send(())
            .expect("observer");
    }
}

/// Draining canceled work must also wait for its uncollected output's destruction.
#[tokio::test]
async fn drain_waits_for_cancelled_output_destruction() {
    let (started, running) = tokio::sync::oneshot::channel();
    let (release_work, work_gate) = std::sync::mpsc::channel();
    let (dropping, destroying) = tokio::sync::oneshot::channel();
    let (release_drop, drop_gate) = std::sync::mpsc::channel();
    let (dropped, destroyed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(run_cpu(move || {
        started.send(()).expect("started");
        work_gate
            .recv_timeout(Duration::from_secs(5))
            .expect("release computation");
        DroppingOutput {
            entered: Some(dropping),
            release: drop_gate,
            finished: Some(dropped),
        }
    }));
    running.await.expect("computation entered");
    task.abort();
    task.await.expect_err("caller canceled");
    release_work.send(()).expect("finish computation");
    destroying.await.expect("output is being destroyed");
    let premature = tokio::time::timeout(
        Duration::from_millis(100),
        docparse_common::drain_cpu(),
    )
    .await;
    // Always release native cleanup before asserting, including on the failing implementation.
    release_drop.send(()).expect("release destruction");
    destroyed.await.expect("output destroyed");
    premature.expect_err("drain returned before discarded output destruction");
    tokio::time::timeout(Duration::from_secs(1), docparse_common::drain_cpu())
        .await
        .expect("drained")
        .expect("CPU pool");
}
