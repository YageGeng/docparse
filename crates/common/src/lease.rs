//! Admission follows the actual resource owner across cancellation and execution boundaries.
use crate::TaskError;
use std::{future::Future, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

tokio::task_local! {
    static CURRENT_RESOURCE: ResourceLease;
}

/// Fixed capacity and shared admission remain one source of truth even while every slot is occupied.
#[derive(Debug)]
pub struct ResourceBudget {
    capacity: usize,
    semaphore: Arc<Semaphore>,
}

impl Clone for ResourceBudget {
    /// Shares permits while preserving the provider's configured scheduling capacity.
    fn clone(&self) -> Self {
        Self {
            capacity: self.capacity,
            semaphore: Arc::clone(&self.semaphore),
        }
    }
}

impl ResourceBudget {
    /// Creates a positive budget suitable for both resource admission and bounded future windows.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "resource capacity must be positive");
        Self {
            capacity,
            semaphore: Arc::new(Semaphore::new(capacity)),
        }
    }

    /// Returns configured capacity, independently of temporary occupancy.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Reserves a fresh unit of work before that invocation allocates its own resources.
    pub async fn reserve(&self) -> Result<ResourceLease, TaskError> {
        ResourceLease::acquire(Arc::clone(&self.semaphore)).await
    }

    /// Stops a provider's admission and wakes producers already waiting for capacity.
    pub fn close(&self) {
        self.semaphore.close();
    }
}

/// Nested model work retains its enclosing document or crop budget until real cleanup finishes.
#[derive(Debug)]
pub struct ResourceLease(Arc<ResourcePermit>);

#[derive(Debug)]
struct ResourcePermit {
    _parent: Option<ResourceLease>,
    _permit: OwnedSemaphorePermit,
}

impl Clone for ResourceLease {
    /// Shares admission without acquiring or duplicating capacity.
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl From<OwnedSemaphorePermit> for ResourceLease {
    /// Captures the enclosing budget when a newly reserved resource begins its lifetime.
    fn from(permit: OwnedSemaphorePermit) -> Self {
        Self(Arc::new(ResourcePermit {
            _parent: Self::current(),
            _permit: permit,
        }))
    }
}

impl ResourceLease {
    /// Waits fairly before the caller allocates an expensive input.
    pub async fn acquire(capacity: Arc<Semaphore>) -> Result<Self, TaskError> {
        capacity
            .acquire_owned()
            .await
            .map(Self::from)
            .map_err(|error| TaskError::from_message(error.to_string()))
    }

    /// Captures resource ownership at task, CPU, image, and request boundaries.
    pub fn current() -> Option<Self> {
        CURRENT_RESOURCE.try_with(Clone::clone).ok()
    }

    /// Returns Tokio's scoped future directly so large pipelines are not stored in an extra async wrapper.
    pub fn scope<F: Future>(
        &self,
        future: F,
    ) -> tokio::task::futures::TaskLocalFuture<Self, F> {
        CURRENT_RESOURCE.scope(self.clone(), future)
    }

    /// Restores resource ownership while a real CPU worker constructs or consumes inputs.
    pub fn scope_sync<F: FnOnce() -> T, T>(&self, operation: F) -> T {
        CURRENT_RESOURCE.sync_scope(self.clone(), operation)
    }
}
