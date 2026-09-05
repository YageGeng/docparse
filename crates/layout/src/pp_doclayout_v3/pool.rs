use std::path::Path;
use std::sync::{Arc, Mutex};

use docparse_config::ExecutionProviderConfig;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{LayoutDetection, LayoutError, PageTransform};

use super::preprocess::ModelInputs;
use super::session::LayoutSession;

/// A bounded collection of independently mutable ONNX Runtime sessions.
pub(crate) struct LayoutSessionPool {
    sessions: Vec<Mutex<LayoutSession>>,
    available: Mutex<Vec<usize>>,
    semaphore: Arc<Semaphore>,
}

impl LayoutSessionPool {
    /// Creates and validates every session before publishing the pool.
    pub(crate) fn new(
        model_path: &Path,
        provider: ExecutionProviderConfig,
        size: usize,
    ) -> Result<Arc<Self>, LayoutError> {
        if size == 0 {
            return Err(LayoutError::SessionPool {
                message: "pool size must be greater than zero".to_owned(),
            });
        }
        let mut sessions = Vec::with_capacity(size);
        for _ in 0..size {
            sessions
                .push(Mutex::new(LayoutSession::load(model_path, provider)?));
        }
        let available = (0..size).rev().collect();
        Ok(Arc::new(Self {
            sessions,
            available: Mutex::new(available),
            semaphore: Arc::new(Semaphore::new(size)),
        }))
    }

    /// Waits asynchronously for one unique session slot without holding a sync lock.
    pub(crate) async fn acquire(
        self: Arc<Self>,
    ) -> Result<LayoutSessionLease, LayoutError> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|source| LayoutError::SessionPool {
                message: source.to_string(),
            })?;
        let index = self
            .available
            .lock()
            .map_err(|source| LayoutError::SessionPool {
                message: source.to_string(),
            })?
            .pop()
            .ok_or_else(|| LayoutError::SessionPool {
                message:
                    "semaphore permit acquired without an available session"
                        .to_owned(),
            })?;
        Ok(LayoutSessionLease {
            pool: self,
            index,
            _permit: permit,
        })
    }
}

/// An RAII lease that returns its pool index and semaphore permit on drop.
pub(crate) struct LayoutSessionLease {
    pool: Arc<LayoutSessionPool>,
    index: usize,
    _permit: OwnedSemaphorePermit,
}

impl LayoutSessionLease {
    /// Runs one page on the uniquely leased mutable session.
    pub(crate) fn detect(
        &self,
        inputs: &ModelInputs,
        transform: &PageTransform,
        threshold: f64,
    ) -> Result<Vec<LayoutDetection>, LayoutError> {
        let session = self.pool.sessions.get(self.index).ok_or_else(|| {
            LayoutError::SessionPool {
                message: format!(
                    "session index {} is out of bounds",
                    self.index
                ),
            }
        })?;
        let mut session =
            session.lock().map_err(|source| LayoutError::SessionPool {
                message: source.to_string(),
            })?;
        session.detect(inputs, transform, threshold)
    }
}

impl Drop for LayoutSessionLease {
    /// Returns the unique index before the owned semaphore permit is released.
    fn drop(&mut self) {
        match self.pool.available.lock() {
            Ok(mut available) => available.push(self.index),
            Err(poisoned) => {
                tracing::warn!(
                    "recovering poisoned layout session availability queue"
                );
                poisoned.into_inner().push(self.index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use docparse_config::ExecutionProviderConfig;

    use super::LayoutSessionPool;

    /// Resolves a repository path from the layout crate directory.
    fn repository_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    /// Verifies a dropped lease returns its unique session slot and semaphore permit.
    #[tokio::test]
    #[ignore = "requires fixed PP-DocLayoutV3 model"]
    async fn lease_drop_returns_session_to_pool() {
        let pool = LayoutSessionPool::new(
            &repository_path("models/pp-doclayout-v3/inference.onnx"),
            ExecutionProviderConfig::Cpu,
            1,
        )
        .expect("the real one-session pool must initialize");
        let first = Arc::clone(&pool)
            .acquire()
            .await
            .expect("the first lease must be available");

        let blocked = tokio::time::timeout(
            Duration::from_millis(20),
            Arc::clone(&pool).acquire(),
        )
        .await;
        let blocked_failure = match blocked {
            Err(_elapsed) => None,
            Ok(_lease) => {
                Some("a second lease unexpectedly acquired a session")
            }
        };
        assert_eq!(blocked_failure, None);

        drop(first);
        let returned = tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&pool).acquire(),
        )
        .await;
        let returned_failure = match returned {
            Ok(Ok(_lease)) => None,
            Ok(Err(error)) => Some(format!("returned lease failed: {error}")),
            Err(error) => Some(format!("returned lease timed out: {error}")),
        };
        assert_eq!(returned_failure, None);
    }
}
