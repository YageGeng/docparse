use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu, StorageSnafu},
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
                break;
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
        self.recover_figures().await
    }

    /// Reconciles only pending attempt directories; committed assets require no database lookup.
    async fn recover_figures(&self) -> ApiResult<()> {
        let mut entries = tokio::fs::read_dir(self.storage.root())
            .await
            .context(StorageSnafu {
                stage: "cleanup-list-figures",
                code: ApiCode::service_unavailable(5031003),
            })?;
        while let Some(entry) =
            entries.next_entry().await.context(StorageSnafu {
                stage: "cleanup-read-figure",
                code: ApiCode::service_unavailable(5031003),
            })?
        {
            let name = entry.file_name();
            let Some((stem, _)) = name
                .to_str()
                .and_then(|name| name.split_once(".json.figures-"))
            else {
                continue;
            };
            let (Some(id), Some(token)) = (
                stem.get(..36).and_then(|id| uuid::Uuid::parse_str(id).ok()),
                stem.get(37..)
                    .and_then(|token| uuid::Uuid::parse_str(token).ok()),
            ) else {
                continue;
            };
            if stem.as_bytes().get(36) != Some(&b'-')
                || !entry
                    .file_type()
                    .await
                    .context(StorageSnafu {
                        stage: "cleanup-stat-figures",
                        code: ApiCode::service_unavailable(5031003),
                    })?
                    .is_dir()
            {
                continue;
            }
            let marker = entry.path().join(".pending");
            if !tokio::fs::try_exists(&marker).await.context(StorageSnafu {
                stage: "cleanup-check-figures",
                code: ApiCode::service_unavailable(5031003),
            })? {
                continue;
            }
            let job = Jobs::find_by_id(&self.db, id).await.with_context(
                |source| DatabaseSnafu {
                    stage: "cleanup-find-figure-job",
                    code: ApiCode::from(&*source),
                },
            )?;
            let result_name = format!("{stem}.json");
            let committed = job.as_ref().is_some_and(|job| {
                job.result_path.as_deref() == Some(result_name.as_str())
            });
            if !committed
                && job.as_ref().is_some_and(|job| {
                    job.status == docparse_database::JobStatus::Running
                        && job.lease_token == Some(token)
                })
            {
                continue;
            }
            // A successful but unacknowledged commit keeps its files; abandoned attempts lose theirs.
            let cleanup = if committed {
                tokio::fs::remove_file(marker).await
            } else {
                tokio::fs::remove_dir_all(entry.path()).await
            };
            if let Err(error) = cleanup
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(error).context(StorageSnafu {
                    stage: "cleanup-recover-figures",
                    code: ApiCode::service_unavailable(5031003),
                });
            }
        }
        Ok(())
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
