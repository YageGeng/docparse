//! Idle queue deadlines must publish telemetry without a new request or an admission read.
use docparse_common::{SessionRequest, queue::BlockingQueue};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::time::{Duration, Instant};

struct Request;
impl SessionRequest for Request {
    /// Keeps the stimulus pending until the test explicitly dequeues it.
    fn cancelled(&self) -> bool {
        false
    }
    /// No per-request timer is attached to this stimulus.
    fn end_queue(&mut self) {}
}

/// Waits for exported state only; calling pressure.paused() would hide a missing timer.
fn wait_for_gauge(handle: &PrometheusHandle, expected: &str) {
    let start = Instant::now();
    loop {
        let metrics = handle.render();
        if metrics.lines().any(|line| {
            line.starts_with("docparse_formula_inline_paused{")
                && line.ends_with(expected)
        }) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "deadline did not publish {expected}: {metrics}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Timer execution is independent of a caller runtime and clears its gauge on shutdown.
#[test]
fn idle_thresholds_publish_pause_and_resume() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).expect("isolated recorder");
    let queue = BlockingQueue::new("deadline_test", 1);
    let pressure = queue.pressure();
    pressure
        .configure(
            0.85,
            0.5,
            Duration::from_millis(30),
            Duration::from_millis(30),
        )
        .expect("pressure timer");
    queue.push(Request).expect("enqueue");
    wait_for_gauge(&handle, " 1");
    assert_eq!(queue.pop(1).expect("dequeue").len(), 1);
    wait_for_gauge(&handle, " 0");
    queue.push(Request).expect("second enqueue");
    wait_for_gauge(&handle, " 1");
    drop(pressure);
    drop(queue);
    wait_for_gauge(&handle, " 0");
    // A discarded queue must not leave a callback that later decrements or resurrects its gauge.
    std::thread::sleep(Duration::from_millis(100));
    wait_for_gauge(&handle, " 0");
    let queue = BlockingQueue::new("deadline_test", 1);
    let pressure = queue.pressure();
    pressure
        .configure(
            0.85,
            0.5,
            Duration::from_millis(200),
            Duration::from_millis(30),
        )
        .expect("cancelable timer");
    queue.push(Request).expect("short overload");
    assert_eq!(queue.pop(1).expect("break high interval").len(), 1);
    std::thread::sleep(Duration::from_millis(250));
    wait_for_gauge(&handle, " 0");
}
