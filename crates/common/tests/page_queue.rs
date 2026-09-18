use docparse_common::{PageQueue, run_cpu};
use std::sync::Arc;
use std::time::Duration;

/// Dequeued work keeps its slot until every real resource holder has acknowledged completion.
#[tokio::test]
async fn cloned_deliveries_keep_capacity_until_the_last_owner_exits() {
    let queue = PageQueue::new(1);
    let lease = queue.reserve().await.expect("slot");
    let peer = lease.clone();
    drop(lease);
    tokio::time::timeout(Duration::from_millis(30), queue.reserve())
        .await
        .expect_err("peer still owns the delivery");
    drop(peer);
    tokio::time::timeout(Duration::from_secs(1), queue.reserve())
        .await
        .expect("capacity restored")
        .expect("open queue");
}

/// Canceling an async caller cannot recycle the page while a blocking operation still owns its resources.
#[tokio::test]
async fn canceled_blocking_work_holds_its_delivery() {
    let queue = PageQueue::new(1);
    let lease = queue.reserve().await.expect("slot");
    let gate =
        Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let worker_gate = Arc::clone(&gate);
    let (started, running) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        lease
            .scope(run_cpu(move || {
                started.send(()).expect("running");
                let mut released = worker_gate.0.lock().expect("gate");
                while !*released {
                    released = worker_gate.1.wait(released).expect("release");
                }
            }))
            .await
    });
    running.await.expect("entered blocking operation");
    task.abort();
    task.await.expect_err("canceled caller");
    let premature =
        tokio::time::timeout(Duration::from_millis(30), queue.reserve()).await;
    *gate.0.lock().expect("gate") = true;
    gate.1.notify_all();
    assert!(
        premature.is_err(),
        "blocking work released its slot too early"
    );
    tokio::time::timeout(Duration::from_secs(1), queue.reserve())
        .await
        .expect("released capacity")
        .expect("queue");
}
