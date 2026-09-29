use docparse_common::ThreadManager;
use std::{cell::RefCell, time::Duration};
use tokio::sync::oneshot;

/// Signals actual OS-thread exit after the worker runtime and its blocking pool have shut down.
struct ThreadExit(Option<oneshot::Sender<()>>);

impl Drop for ThreadExit {
    /// A thread-local destructor distinguishes future completion from thread termination.
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

thread_local! {
    static EXIT: RefCell<Option<ThreadExit>> = const { RefCell::new(None) };
}

/// Both implicit and explicit cleanup must avoid joining the async worker from its own runtime.
#[tokio::test]
async fn async_workers_can_release_their_own_thread_owner() {
    for explicit in [false, true] {
        let (owner, delivered) = oneshot::channel::<ThreadManager>();
        let (exited, observed) = oneshot::channel();
        let (done, completed) = oneshot::channel();
        let worker = ThreadManager::spawn_async(Box::pin(async move {
            EXIT.with(|slot| {
                *slot.borrow_mut() = Some(ThreadExit(Some(exited)))
            });
            let worker = delivered.await.expect("thread owner");
            if explicit {
                worker.shutdown().await.expect("explicit cleanup");
            } else {
                drop(worker);
            }
            done.send(()).expect("future completion");
        }))
        .expect("worker");
        owner
            .send(worker)
            .map_err(|_undelivered| "worker stopped before ownership")
            .expect("live worker");
        tokio::time::timeout(Duration::from_secs(1), async {
            completed.await.expect("worker future finished");
            observed.await.expect("OS thread terminated");
        })
        .await
        .expect("self cleanup must not create a join cycle");
    }
}
