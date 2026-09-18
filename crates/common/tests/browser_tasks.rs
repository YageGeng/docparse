//! Runs the production browser task owner on a local executor without loading browser models.
extern crate self as wasm_bindgen_futures;

use std::{future::Future, time::Duration};
use tokio::{sync::oneshot, task::LocalSet};

pub use docparse_common::{PageLease, TaskError};
/// Mirrors the browser's non-Send boxed future while exercising its production scheduler on LocalSet.
pub type WasmBoxedFuture<'a, T> =
    std::pin::Pin<Box<dyn Future<Output = T> + 'a>>;

#[path = "../src/task_set/web.rs"]
mod browser;

/// Substitutes only the browser microtask executor; task ownership uses production code unchanged.
pub fn spawn_local<F: Future<Output = ()> + 'static>(future: F) {
    tokio::task::spawn_local(future);
}

/// A full downstream stage must not prevent an admitted upstream task from running.
#[tokio::test]
async fn tasks_run_before_results_are_collected() {
    LocalSet::new()
        .run_until(async {
            let mut tasks = browser::TaskSet::new();
            let (started, receiver) = oneshot::channel();
            tasks.spawn(async move {
                let _ = started.send(());
                42
            });
            tokio::time::timeout(Duration::from_secs(1), receiver)
                .await
                .expect("task must execute without join_next")
                .expect("task started");
            // Completed but uncollected results still count against the stage's memory budget.
            assert_eq!(tasks.len(), 1);
            assert_eq!(
                tasks.join_next().await.expect("result").expect("task"),
                42
            );
            assert!(tasks.is_empty());
        })
        .await;
}

/// Dropping a completion before polling it must cancel the owned producer before it runs.
#[tokio::test]
async fn dropping_unpolled_completion_cancels_task() {
    LocalSet::new()
        .run_until(async {
            let (started, receiver) = oneshot::channel();
            let completion = browser::spawn(async move {
                let _ = started.send(());
            });
            drop(completion);
            assert!(
                tokio::time::timeout(Duration::from_secs(1), receiver)
                    .await
                    .expect("cancelled task must release its captures")
                    .is_err()
            );
        })
        .await;
}

/// Aborting or dropping the collection cancels running callers and allows reuse after abort_all.
#[tokio::test]
async fn cancelling_collections_releases_running_tasks() {
    LocalSet::new()
        .run_until(async {
            for abort in [true, false] {
                let mut tasks = browser::TaskSet::new();
                let (started, entered) = oneshot::channel();
                let (lifetime, released) = oneshot::channel::<()>();
                tasks.spawn(async move {
                    // Keep this sender alive until cancellation actually drops the inner future.
                    let _lifetime = lifetime;
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                });
                tokio::time::timeout(Duration::from_secs(1), entered)
                    .await
                    .expect("task must start")
                    .expect("started");
                if abort {
                    tasks.abort_all();
                    assert!(tasks.is_empty());
                } else {
                    drop(tasks);
                    tasks = browser::TaskSet::new();
                }
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), released)
                        .await
                        .expect("running task must release its captures")
                        .is_err()
                );
                tasks.spawn(async {});
                tasks
                    .join_next()
                    .await
                    .expect("new result")
                    .expect("reused collection");
            }
        })
        .await;
}
