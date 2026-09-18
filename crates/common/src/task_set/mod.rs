//! Platform declarations and exports for owned task scheduling.
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{TaskSet, spawn};

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::{TaskSet, spawn};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    /// Dropping an unpolled completion must cancel owned work instead of detaching a runtime task.
    #[tokio::test]
    async fn dropping_completion_cancels_unpolled_work() {
        let ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&ran);
        let completion = spawn(async move {
            observed.store(true, Ordering::SeqCst);
        });
        drop(completion);
        tokio::task::yield_now().await;
        assert!(!ran.load(Ordering::SeqCst));
    }

    /// Task groups collect every result without imposing submission order on completion.
    #[tokio::test]
    async fn task_groups_collect_all_owned_results() {
        let mut tasks = TaskSet::new();
        for value in 0..3 {
            tasks.spawn(async move { value });
        }
        assert_eq!(tasks.len(), 3);
        let mut values = Vec::new();
        while let Some(result) = tasks.join_next().await {
            values.push(result.expect("task"));
        }
        values.sort_unstable();
        assert_eq!(values, [0, 1, 2]);
        assert!(tasks.is_empty());
    }
}
