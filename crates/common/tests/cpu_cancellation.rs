use docparse_common::run_cpu;
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Wake, Waker},
    time::Duration,
};

/// Records completion without consuming the CPU task's returned value.
struct Completion(tokio::sync::Notify);

impl Wake for Completion {
    /// Retains the completion signal even when the observer has not started waiting.
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }

    /// Signals that the completed JoinHandle can now be canceled without polling it again.
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

/// Reports which thread destroys a canceled input or an uncollected result.
struct DropThread(Option<tokio::sync::oneshot::Sender<std::thread::ThreadId>>);

impl Drop for DropThread {
    /// Reports affinity through the originating runtime, which cleanup must preserve for follow-up tasks.
    fn drop(&mut self) {
        let observer = self.0.take().expect("drop observer");
        let thread = std::thread::current().id();
        tokio::spawn(async move {
            let _ = observer.send(thread);
        });
    }
}

/// Both completed results and inputs awaiting admission must be destroyed outside the executor.
#[tokio::test]
#[allow(clippy::panic)] // Executing an operation canceled before admission must fail the regression.
async fn cancellation_drops_completed_results_and_waiting_inputs_off_executor()
{
    let caller = std::thread::current().id();
    let (release, gate) = std::sync::mpsc::channel();
    let (observed, dropped) = tokio::sync::oneshot::channel();
    let completion = Arc::new(Completion(tokio::sync::Notify::new()));
    let waker = Waker::from(Arc::clone(&completion));
    let mut task = Box::pin(run_cpu(move || {
        gate.recv_timeout(Duration::from_secs(5))
            .expect("release computation");
        DropThread(Some(observed))
    }));
    assert!(
        task.as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    release.send(()).expect("finish computation");
    tokio::time::timeout(Duration::from_secs(5), completion.0.notified())
        .await
        .expect("output published");
    // The output is already in the JoinHandle; dropping it reproduces the cancellation race exactly.
    drop(task);
    let completed_thread =
        tokio::time::timeout(Duration::from_secs(5), dropped)
            .await
            .expect("output cleanup")
            .expect("drop thread");

    let capacity = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .saturating_sub(1)
        .max(1);
    let gate =
        Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let (started, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..capacity {
        let gate = Arc::clone(&gate);
        let started = started.clone();
        tasks.spawn(run_cpu(move || {
            started.send(()).expect("CPU slot entered");
            let lock = gate.0.lock().expect("gate");
            let (released, _) = gate
                .1
                .wait_timeout_while(lock, Duration::from_secs(5), |released| {
                    !*released
                })
                .expect("gate release");
            assert!(*released, "CPU work must be released");
        }));
    }
    for _ in 0..capacity {
        entered.recv().await.expect("occupied CPU slot");
    }
    let (observed, dropped) = tokio::sync::oneshot::channel();
    let input = DropThread(Some(observed));
    let mut waiting = Box::pin(run_cpu(move || {
        drop(input);
        panic!("canceled operation must not execute");
    }));
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    drop(waiting);
    *gate.0.lock().expect("gate") = true;
    gate.1.notify_all();
    while let Some(task) = tasks.join_next().await {
        task.expect("caller").expect("CPU work");
    }
    docparse_common::drain_cpu().await.expect("cleanup drained");
    // Drain must cover queued-input cleanup too, rather than just admitted operations.
    let waiting_thread = dropped.await.expect("input cleanup");
    assert_ne!(
        completed_thread, caller,
        "completed output cleanup ran on the async executor"
    );
    assert_ne!(
        waiting_thread, caller,
        "waiting input cleanup ran on the async executor"
    );
}
