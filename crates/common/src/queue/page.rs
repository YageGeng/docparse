//! Completion-counted page deliveries, independent of PDFium and model types.
use crate::TaskError;
use std::{future::Future, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

tokio::task_local! {
    static CURRENT_PAGE: PageLease;
}

/// Shared render capacity; receiving a page does not acknowledge its delivery.
#[derive(Debug, Clone)]
pub struct PageQueue(Arc<Semaphore>);

impl PageQueue {
    /// Allocates the configured positive number of unfinished page slots.
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "page queue capacity must be positive");
        Self(Arc::new(Semaphore::new(size)))
    }

    /// Reserves capacity before rendering rather than after pixels have already been allocated.
    pub async fn reserve(&self) -> Result<PageLease, TaskError> {
        Arc::clone(&self.0)
            .acquire_owned()
            .await
            .map(|permit| PageLease(Arc::new(permit)))
            .map_err(|error| TaskError::from_message(error.to_string()))
    }
}

/// Every actual resource owner shares one delivery; the final drop acknowledges completion.
#[derive(Debug)]
pub struct PageLease(Arc<OwnedSemaphorePermit>);

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
