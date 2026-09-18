#![cfg(target_os = "linux")]

use docparse_common::ThreadManager;
use std::{process::Command, time::Duration};

/// A thread quota failure must return TaskError without destroying a nested runtime on the async caller.
#[test]
fn native_spawn_failure_returns_error() {
    const CHILD: &str = "DOCPARSE_TEST_THREAD_SPAWN_FAILURE";
    if std::env::var_os(CHILD).is_none() {
        let output =
            Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "native_spawn_failure_returns_error",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .expect("isolated test process");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // Apply the quota after libtest has started its test thread, and only to this disposable process.
    let limited = Command::new("prlimit")
        .args(["--pid", &std::process::id().to_string(), "--nproc=0:0"])
        .status();
    match limited {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping thread-quota regression: prlimit is unavailable"
            );
            return;
        }
        result => assert!(result.expect("set child quota").success()),
    }
    // Privileged users can bypass RLIMIT_NPROC; those environments cannot exercise this failure path.
    if let Ok(thread) = std::thread::Builder::new().spawn(|| ()) {
        thread.join().expect("quota probe");
        eprintln!(
            "skipping thread-quota regression: the process quota is not enforced"
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("caller runtime");
    let result = runtime
        .block_on(async { ThreadManager::spawn_async(Box::pin(async {})) });
    assert!(
        result.is_err(),
        "thread creation must fail through the Result API"
    );
}

/// The startup acknowledgement must leave the initialized worker usable after its caller runtime exits.
#[test]
fn async_worker_survives_construction_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("caller runtime");
    let (send, receive) = tokio::sync::oneshot::channel();
    let (completed, result) = std::sync::mpsc::channel();
    let owner = runtime
        .block_on(async {
            ThreadManager::spawn_async(Box::pin(async move {
                completed
                    .send(receive.await.expect("input"))
                    .expect("result");
            }))
        })
        .expect("worker initialization");
    drop(runtime);
    send.send(42).expect("live worker");
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(2))
            .expect("completion"),
        42
    );
    drop(owner);
}
