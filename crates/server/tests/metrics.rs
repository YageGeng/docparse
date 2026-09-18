//! Recorder-backed checks exercise real shared queue ownership without loading models.
// Occupancy counters are exact integer contributions represented as f64.
#![allow(clippy::float_cmp)]
use docparse_common::{PageQueue, Queue, SessionRequest, queue::BlockingQueue};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

struct Request;
impl SessionRequest for Request {
    /// Keeps the test focused on queue ownership rather than reply cancellation.
    fn cancelled(&self) -> bool {
        false
    }
    /// No per-request timing observer is needed for recorder assertions.
    fn end_queue(&mut self) {}
}

/// Reads a unique numeric series from the real exporter using the same parser as the live UI.
fn sample(handle: &PrometheusHandle, name: &str) -> f64 {
    prometheus_parse::Scrape::parse(
        handle.render().lines().map(|line| Ok(line.to_owned())),
    )
    .expect("scrape")
    .samples
    .into_iter()
    .find_map(|sample| {
        if sample.metric != name {
            return None;
        }
        match sample.value {
            prometheus_parse::Value::Gauge(value)
            | prometheus_parse::Value::Counter(value)
            | prometheus_parse::Value::Untyped(value) => Some(value),
            _ => None,
        }
    })
    .expect("registered metric")
}

/// Cloned page owners must release one slot only after the final owner disappears.
#[test]
fn final_page_owner_and_queue_closure_release_gauges() {
    let recorder = PrometheusBuilder::new()
        .set_buckets(&[0.1, 1.0])
        .expect("buckets")
        .build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let pages = PageQueue::new(1);
            let lease = pages.reserve().await.expect("lease");
            let clone = lease.clone();
            drop(lease);
            drop(pages);
            assert_eq!(sample(&handle, "docparse_page_slots_used"), 1.0);
            assert_eq!(sample(&handle, "docparse_page_slots_capacity"), 1.0);
            drop(clone);
            assert_eq!(sample(&handle, "docparse_page_slots_used"), 0.0);
            assert_eq!(sample(&handle, "docparse_page_slots_capacity"), 0.0);
            assert_eq!(
                sample(&handle, "docparse_page_hold_seconds_count"),
                1.0
            );
        });
        let queue = BlockingQueue::new("test", 2);
        queue.push(Request).expect("admit");
        assert_eq!(sample(&handle, "docparse_queue_items"), 1.0);
        queue.close();
        assert_eq!(sample(&handle, "docparse_queue_items"), 0.0);
        assert_eq!(sample(&handle, "docparse_queue_removed_items_total"), 1.0);
        drop(queue);
        assert_eq!(sample(&handle, "docparse_queue_capacity_items"), 0.0);
    });
}

/// Cancelled producers cannot leak occupancy, and a closed async receiver discards queued work.
#[test]
fn asynchronous_backpressure_counts_only_admitted_inputs() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (sender, queue) = Queue::new("test", 1);
            sender.send(Request).await.expect("first admission");
            let pending = sender.send(Request);
            tokio::pin!(pending);
            tokio::select! {
                _ = &mut pending => panic!("full queue admitted a second input"),
                _ = tokio::task::yield_now() => {}
            }
            assert_eq!(sample(&handle, "docparse_queue_items"), 1.0);
            assert_eq!(sample(&handle, "docparse_queue_blocked_producers"), 1.0);
            drop(queue);
            assert!(pending.await.is_err());
            assert_eq!(sample(&handle, "docparse_queue_items"), 0.0);
            assert_eq!(sample(&handle, "docparse_queue_blocked_producers"), 0.0);
        });
    });
}

/// Dropping a capacity wait must observe elapsed time without leaking a slot or blocked producer.
#[test]
fn cancelled_admission_records_model_and_page_waits() {
    let recorder = PrometheusBuilder::new()
        .set_buckets(&[0.001, 1.0])
        .expect("buckets")
        .build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (sender, _queue) = Queue::new("cancel_test", 1);
            sender.send(Request).await.expect("fill queue");
            tokio::time::timeout(
                std::time::Duration::from_millis(5),
                sender.send(Request),
            )
            .await
            .expect_err("capacity wait timed out");
            let pages = PageQueue::new(1);
            let _lease = pages.reserve().await.expect("fill page capacity");
            tokio::time::timeout(
                std::time::Duration::from_millis(5),
                pages.reserve(),
            )
            .await
            .expect_err("capacity wait timed out");
            assert_eq!(
                sample(&handle, "docparse_queue_blocked_producers"),
                0.0
            );
            assert_eq!(sample(&handle, "docparse_page_blocked_producers"), 0.0);
            let scrape = prometheus_parse::Scrape::parse(
                handle.render().lines().map(|line| Ok(line.to_owned())),
            )
            .expect("scrape");
            for name in [
                "docparse_queue_admission_wait_seconds_count",
                "docparse_page_admission_wait_seconds_count",
            ] {
                assert!(
                    scrape.samples.iter().any(|sample| sample.metric == name
                        && sample.labels.get("outcome") == Some("cancelled")),
                    "missing cancelled wait for {name}"
                );
            }
        });
    });
}

/// A detached blocking publisher must retain the timer until its real work releases ownership.
#[test]
fn publishing_timer_outlives_cancelled_async_waiter() {
    let recorder = PrometheusBuilder::new()
        .set_buckets(&[0.001, 1.0])
        .expect("buckets")
        .build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let timer = docparse_common::telemetry::Timer::new(
                "test_publish_seconds",
                "scope",
                "local",
            );
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                // Match publication's ownership handoff: blocking work returns the timer with its output.
                let output = tokio::task::spawn_blocking(move || {
                    let timer = timer;
                    started.send(()).expect("started");
                    wait.blocking_recv().expect("release writer");
                    ((), timer)
                })
                .await
                .expect("writer");
                drop(output);
            });
            ready.await.expect("writer entered");
            task.abort();
            assert!(task.await.expect_err("cancelled waiter").is_cancelled());
            assert_eq!(sample(&handle, "test_publish_seconds_count"), 0.0);
            release.send(()).expect("finish actual work");
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while sample(&handle, "test_publish_seconds_count") == 0.0 {
                    tokio::time::sleep(std::time::Duration::from_millis(1))
                        .await;
                }
            })
            .await
            .expect("released timer");
            assert_eq!(sample(&handle, "test_publish_seconds_count"), 1.0);
        });
    });
}

/// Native, browser and remote executors share one pool lifetime and one set of consumer accounting rules.
#[test]
fn consumer_metrics_follow_pool_and_worker_lifetimes() {
    let recorder = PrometheusBuilder::new()
        .set_buckets(&[0.001, 1.0])
        .expect("buckets")
        .build_recorder();
    let handle = recorder.handle();
    metrics::with_local_recorder(&recorder, || {
        let pool = docparse_common::telemetry::ModelMetrics::new("test", 2, 4);
        let worker = std::sync::Arc::clone(&pool);
        drop(pool);
        assert_eq!(sample(&handle, "docparse_model_workers_configured"), 2.0);
        assert_eq!(sample(&handle, "docparse_model_batch_limit"), 4.0);
        assert_eq!(sample(&handle, "docparse_model_workers_alive"), 0.0);
        let alive = worker.alive(2);
        let batch = worker.batch();
        assert_eq!(sample(&handle, "docparse_model_workers_alive"), 2.0);
        assert_eq!(sample(&handle, "docparse_model_workers_busy"), 1.0);
        drop(batch);
        assert_eq!(sample(&handle, "docparse_model_workers_busy"), 0.0);
        assert_eq!(
            sample(&handle, "docparse_model_batch_service_seconds_count"),
            1.0
        );
        drop(alive);
        drop(worker);
        assert_eq!(sample(&handle, "docparse_model_workers_alive"), 0.0);
        assert_eq!(sample(&handle, "docparse_model_workers_configured"), 0.0);
        assert_eq!(sample(&handle, "docparse_model_batch_limit"), 0.0);
    });
}
