//! Completion-counted page deliveries, independent of PDFium and model types.
use crate::{
    TaskError,
    telemetry::{Activity, Admission},
};
use std::{future::Future, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use web_time::Instant;

tokio::task_local! {
    static CURRENT_PAGE: PageLease;
}

/// Shared render capacity; receiving a page does not acknowledge its delivery.
#[derive(Debug, Clone)]
pub struct PageQueue(Arc<PageCapacity>);

/// Capacity remains registered while any delivery still owns a permit.
#[derive(Debug)]
struct PageCapacity {
    semaphore: Arc<Semaphore>,
    _capacity: Activity,
}

/// Derived crops share one occupied slot and one final completion observation.
#[derive(Debug)]
struct PagePermit {
    _permit: OwnedSemaphorePermit,
    _owner: Arc<PageCapacity>,
    started: Instant,
}
impl Drop for PagePermit {
    /// Records the complete resource lifetime rather than only rendering or receiving the page.
    fn drop(&mut self) {
        metrics::gauge!("docparse_page_slots_used").decrement(1.0);
        metrics::histogram!("docparse_page_hold_seconds")
            .record(self.started.elapsed().as_secs_f64());
    }
}

impl PageQueue {
    /// Allocates the configured positive number of unfinished page slots.
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "page queue capacity must be positive");
        Self(Arc::new(PageCapacity {
            semaphore: Arc::new(Semaphore::new(size)),
            _capacity: Activity::new(
                "docparse_page_slots_capacity",
                ("pool", "render"),
                size as f64,
            ),
        }))
    }

    /// Reserves capacity before rendering rather than after pixels have already been allocated.
    pub async fn reserve(&self) -> Result<PageLease, TaskError> {
        let admission =
            Admission::new("docparse_page_admission_wait_seconds", "render");
        let permit = match Arc::clone(&self.0.semaphore).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let _blocked = Activity::new(
                    "docparse_page_blocked_producers",
                    ("pool", "render"),
                    1.0,
                );
                Arc::clone(&self.0.semaphore)
                    .acquire_owned()
                    .await
                    .map_err(|error| {
                        TaskError::from_message(error.to_string())
                    })?
            }
        };
        admission.finish("admitted");
        metrics::gauge!("docparse_page_slots_used").increment(1.0);
        Ok(PageLease(Arc::new(PagePermit {
            _permit: permit,
            _owner: Arc::clone(&self.0),
            started: Instant::now(),
        })))
    }
}

/// Every actual resource owner shares one delivery; the final drop acknowledges completion.
#[derive(Debug)]
pub struct PageLease(Arc<PagePermit>);

impl Clone for PageLease {
    /// Shares one slot without acquiring another permit for derived work.
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl PageLease {
    /// Captures resource ownership at execution boundaries independently of timing or tracing.
    pub fn current() -> Option<Self> {
        CURRENT_PAGE.try_with(Clone::clone).ok()
    }

    /// Makes nested model requests and CPU submissions retain this page's delivery.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        CURRENT_PAGE.scope(self.clone(), future).await
    }

    /// Restores page ownership while an actual blocking worker prepares or consumes resources.
    pub fn scope_sync<F: FnOnce() -> T, T>(&self, operation: F) -> T {
        CURRENT_PAGE.sync_scope(self.clone(), operation)
    }
}
