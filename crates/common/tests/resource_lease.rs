use docparse_common::{ResourceLease, run_cpu};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

/// Provider clones keep the configured window under full occupancy and close wakes their shared waiters.
#[tokio::test]
async fn provider_budget_keeps_capacity_and_closes_shared_admission() {
    let budget = docparse_common::ResourceBudget::new(2);
    let peer = budget.clone();
    let _first = budget.reserve().await.expect("first resource");
    let _second = peer.reserve().await.expect("second resource");
    assert_eq!(peer.capacity(), 2);
    let mut waiting = Box::pin(peer.reserve());
    assert!(futures_util::poll!(&mut waiting).is_pending());
    budget.close();
    waiting.await.expect_err("closed provider wakes its waiter");
}

/// Cancelled CPU work must retain both its model budget and its enclosing document budget.
#[tokio::test]
async fn cancelled_cpu_work_retains_nested_resource_budgets() {
    let documents = Arc::new(Semaphore::new(1));
    let crops = Arc::new(Semaphore::new(1));
    let document = ResourceLease::acquire(Arc::clone(&documents))
        .await
        .expect("document");
    let crop_budget = Arc::clone(&crops);
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        document
            .scope(async move {
                let crop =
                    ResourceLease::acquire(crop_budget).await.expect("crop");
                crop.scope(run_cpu(move || {
                    entered.send(()).expect("entered");
                    wait.recv_timeout(Duration::from_secs(5)).expect("release");
                }))
                .await
            })
            .await
    });
    started.await.expect("CPU running");
    task.abort();
    task.await.expect_err("caller cancelled");
    let retained = (documents.available_permits(), crops.available_permits());
    release.send(()).expect("release CPU");
    docparse_common::drain_cpu().await.expect("CPU cleanup");
    assert_eq!(
        retained,
        (0, 0),
        "real work must retain every enclosing budget"
    );
    assert_eq!(
        (documents.available_permits(), crops.available_permits()),
        (1, 1)
    );
}

/// An abandoned blocking output models a temporary file whose destructor still performs real cleanup.
#[derive(Debug)]
struct Cleanup {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}
impl Drop for Cleanup {
    /// Keeps destruction observable until the test allows actual cleanup to complete.
    fn drop(&mut self) {
        self.entered
            .take()
            .expect("cleanup signal")
            .send(())
            .expect("cleanup entered");
        self.release
            .recv_timeout(Duration::from_secs(5))
            .expect("release cleanup");
    }
}

/// Cancellation cannot recycle publication capacity before an uncollected filesystem result is destroyed.
#[tokio::test]
async fn abandoned_blocking_output_retains_admission_through_cleanup() {
    let capacity = Arc::new(Semaphore::new(1));
    let resources = ResourceLease::acquire(Arc::clone(&capacity))
        .await
        .expect("admission");
    let (entered, started) = tokio::sync::oneshot::channel();
    let (finish, waiting) = std::sync::mpsc::channel();
    let (cleaning, cleanup) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        resources
            .scope(docparse_common::run_blocking(move || {
                entered.send(()).expect("operation entered");
                waiting
                    .recv_timeout(Duration::from_secs(5))
                    .expect("finish operation");
                Cleanup {
                    entered: Some(cleaning),
                    release: released,
                }
            }))
            .await
    });
    started.await.expect("running");
    task.abort();
    task.await.expect_err("cancelled caller");
    finish.send(()).expect("finish operation");
    cleanup.await.expect("cleanup entered");
    let premature = capacity.available_permits();
    release.send(()).expect("complete cleanup");
    let _released = tokio::time::timeout(
        Duration::from_secs(1),
        Arc::clone(&capacity).acquire_owned(),
    )
    .await
    .expect("cleanup finished")
    .expect("open budget");
    assert_eq!(
        premature, 0,
        "uncollected output released publication capacity before cleanup"
    );
}
