use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu},
    state::AppState,
    storage::SharedStorage,
};
use docparse_database::{
    entities::parse_jobs::Model, query::parse_job::ParseJobQuery as Jobs,
    seaorm::DatabaseConnection,
};
use snafu::ResultExt;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Recovers deleted result files from durable database markers independently of HTTP requests and GPU work.
pub struct DeletedResults {
    db: DatabaseConnection,
    storage: SharedStorage,
}

impl From<&AppState> for DeletedResults {
    /// Shares the configured pool and storage without retaining upload or parser resources.
    fn from(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            storage: state.storage.clone(),
        }
    }
}

impl DeletedResults {
    /// Cleans one committed deletion; interruption leaves its path available for an idempotent retry.
    pub async fn clean(&self, job: &Model) -> ApiResult<()> {
        if job.deleted_at.is_none() {
            return RequestSnafu {
                stage: "cleanup-check-deletion",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            }
            .fail();
        }
        let Some(name) = job.result_path.as_deref() else {
            return Ok(());
        };
        // No database connection or transaction is held while the shared filesystem is accessed.
        tokio::time::timeout(Duration::from_secs(5), async {
            self.storage.remove(name).await?;
            Jobs::finish_cleanup(&self.db, job.id, name)
                .await
                .with_context(|source| DatabaseSnafu {
                    stage: "cleanup-ack-result",
                    code: ApiCode::from(&*source),
                })?;
            Ok(())
        })
        .await
        .map_err(|_elapsed| {
            RequestSnafu {
                stage: "cleanup-result-timeout",
                code: ApiCode::service_unavailable(5031003),
            }
            .build()
        })?
    }

    /// Scans pending deletions in bounded batches and keeps failed items durable for the next sweep.
    pub async fn sweep(&self) -> ApiResult<()> {
        let mut cursor = None;
        loop {
            let jobs = Jobs::pending_cleanup(&self.db, cursor)
                .await
                .with_context(|source| DatabaseSnafu {
                    stage: "cleanup-list-pending",
                    code: ApiCode::from(&*source),
                })?;
            if jobs.is_empty() {
                return Ok(());
            }
            for job in jobs {
                cursor = Some(job.id);
                match self.clean(&job).await {
                    Ok(()) => tracing::info!(
                        "cleaned deleted result for job {}",
                        job.id
                    ),
                    Err(error) => tracing::warn!(
                        "result cleanup for job {} remains pending: {}",
                        job.id,
                        error
                    ),
                }
            }
        }
    }

    /// Retries cleanup at startup and every thirty seconds in every server role, stopping promptly on shutdown.
    pub async fn run(self, shutdown: CancellationToken) {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { _ = shutdown.cancelled() => return, _ = tick.tick() => {} }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                result = self.sweep() => if let Err(error) = result {
                    tracing::warn!("result cleanup sweep failed; retrying: {}", error);
                }
            }
        }
    }
}
