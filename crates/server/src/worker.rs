use crate::{
    code::ApiCode,
    error::{
        ApiResult, DatabaseSnafu, ParseSnafu, RequestSnafu, SerializeSnafu,
        StorageSnafu, TaskSnafu,
    },
    model::{base::ApiResponse, error::ErrorCode},
    storage::SharedStorage,
};
use docparse_config::OutputConfig;
use docparse_core::{DocParser, ParseObserver, ParseOptions, ParseProgress};
use docparse_database::{
    query::parse_job::{Lease, ParseJobQuery as Jobs},
    seaorm::DatabaseConnection,
};
use futures_util::TryFutureExt;
use snafu::ResultExt;
use std::{io::Write, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, instrument::WithSubscriber};
use typed_builder::TypedBuilder;

/// Worker limits bound parser memory separately from HTTP concurrency and persisted queue length.
#[derive(Clone, TypedBuilder)]
pub struct WorkerOptions {
    #[builder(default = 2)]
    pub concurrency: usize,
    #[builder(default = 60)]
    pub lease_seconds: i32,
    #[builder(default = 3)]
    pub max_attempts: i32,
    #[builder(default = Duration::from_secs(3600))]
    pub job_timeout: Duration,
}

impl WorkerOptions {
    /// Validates supervision budgets for both queue workers and explicitly invoked attempts.
    pub fn validate(&self) -> ApiResult<()> {
        if self.concurrency == 0
            || self.concurrency > 128
            || !(3..=86400).contains(&self.lease_seconds)
            || !(1..=100).contains(&self.max_attempts)
            || self.job_timeout.is_zero()
        {
            return RequestSnafu {
                stage: "worker-validate-options",
                code: ApiCode::COMMON_BAD_REQUEST,
            }
            .fail();
        }
        Ok(())
    }
}

/// Reusable parser engines are loaded once and shared by independently leased document attempts.
#[derive(Clone, TypedBuilder)]
pub struct Worker {
    pub db: DatabaseConnection,
    pub storage: SharedStorage,
    pub parser: Arc<DocParser>,
    pub output: OutputConfig,
    pub options: WorkerOptions,
}

/// A watch channel replaces obsolete progress snapshots instead of blocking inference on slow clients.
struct ProgressObserver(watch::Sender<Option<ParseProgress>>);

impl ParseObserver for ProgressObserver {
    /// Sends progress synchronously into one replaceable slot, independent of database and SSE latency.
    fn on_progress(&self, progress: ParseProgress) {
        self.0.send_replace(Some(progress));
    }

    /// Surfaces slow completed stages at the default log level, retaining the current job correlation.
    fn on_timing(&self, timing: docparse_core::Timing) {
        // Short per-crop stages remain DEBUG-only in the shared timer to keep production logs bounded.
        if timing.duration_ms >= 1000.0 {
            tracing::info!(
                "slow parse stage {:?} for page {:?} elapsed {:.3} ms",
                timing.stage,
                timing.page_number,
                timing.duration_ms
            );
        }
    }
}

impl Worker {
    /// Stops claiming on shutdown and drains already accepted attempts; forced exits recover by lease expiry.
    pub async fn run(self, shutdown: CancellationToken) -> ApiResult<()> {
        self.options.validate()?;
        let mut tasks = JoinSet::new();
        loop {
            if shutdown.is_cancelled() && tasks.is_empty() {
                return Ok(());
            }
            if !shutdown.is_cancelled()
                && tasks.len() < self.options.concurrency
            {
                match Jobs::claim(
                    &self.db,
                    self.options.lease_seconds,
                    self.options.max_attempts,
                )
                .await
                {
                    Ok(Some(lease)) => {
                        let worker = self.clone();
                        tasks.spawn(
                            async move { worker.process(lease).await }
                                .with_current_subscriber(),
                        );
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!("task claim failed; retrying: {}", error)
                    }
                }
            }
            tokio::select! {
                result = tasks.join_next(), if !tasks.is_empty() => {
                    // Ordinary attempt failures are logged inside their PDF span before returning here.
                    if let Some(Err(_)) = result {
                        tracing::error!("worker task panicked or was cancelled; its lease will expire");
                    }
                }
                _ = shutdown.cancelled(), if !shutdown.is_cancelled() => {
                    tracing::info!("worker is draining {} in-flight jobs", tasks.len());
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    /// Parses one fenced attempt while renewing its lease through PDF, inference, and result publication.
    pub async fn process(&self, lease: Lease) -> ApiResult<()> {
        // Recreate a local span from durable fields for every attempt; no process-local Span ID is persisted.
        let span = tracing::info_span!(target: crate::logging::CONTEXT_TARGET, parent: None, "pdf_parse",
            job_id = %lease.job.id, pdf_hash = %lease.job.input_hash, attempt = lease.job.attempts);
        async move {
            // Construct business APICODEs in each context so persisted messages retain the same contract as HTTP responses.
            self.options.validate()?;
            let id = lease.job.id;
            tracing::info!("starting job {} attempt {}", id, lease.job.attempts);
            let started = tokio::time::Instant::now();
            let (sender, mut progress) = watch::channel(None);
            let observer = ProgressObserver(sender);
            // Own the parse in a separate task: synchronous document work must not stop the lease watchdog.
            // JoinSet aborts this owned task if supervision is cancelled or loses its database lease.
            let parser = Arc::clone(&self.parser);
            let storage = self.storage.clone();
            let output = self.output.clone();
            let input_hash = lease.job.input_hash.clone();
            let token = lease.token;
            let mut operation = JoinSet::new();
            operation.spawn(async move {
                let input = storage.path(&format!("{input_hash}.pdf"))?;
                let result = parser
                    .parse_path_with_options(
                        input,
                        ParseOptions::builder().observer(Some(&observer)).build(),
                    )
                    .await
                    .context(ParseSnafu {
                        stage: "document-parse-pdf",
                        code: ApiCode::unprocessable_entity(4221001),
                    })?;
                let mut temporary = storage.temporary().await?;
                let writer_span = tracing::Span::current();
                let writer_dispatcher = tracing::dispatcher::get_default(Clone::clone);
                let temporary =
                    tokio::task::spawn_blocking(move || tracing::dispatcher::with_default(&writer_dispatcher, || writer_span.in_scope(|| -> ApiResult<_> {
                        // Stream the standard API envelope around the configured canonical view, avoiding a second document-sized buffer.
                        {
                            // Buffer small serializer writes so large documents do not issue one filesystem write per token.
                            let mut writer =
                                std::io::BufWriter::new(temporary.as_file_mut());
                            // Borrow the configured document view and let the shared response type own the wire format.
                            ApiResponse::data(
                                docparse_core::JsonRenderer::view_with_config(
                                    &result, &output,
                                ),
                            )
                            .write(&mut writer)
                            .context(SerializeSnafu {
                                stage: "result-serialize-json",
                                code: ApiCode::COMMON_INTERNAL_ERROR,
                            })?;
                            writer.flush().context(StorageSnafu {
                                stage: "result-flush-file",
                                code: ApiCode::service_unavailable(5031003),
                            })?;
                        }
                        Ok(temporary)
                    })))
                    .await
                    .context(TaskSnafu {
                        stage: "result-write-task",
                        code: ApiCode::COMMON_INTERNAL_ERROR,
                    })??;
                // Each attempt gets a different object: a stale blocking writer cannot replace its successor's result.
                let name = format!("{id}-{token}.json");
                storage.publish(temporary, &name).await?;
                Ok::<_, crate::error::ApiError>(name)
            }.in_current_span().with_current_subscriber());
            let deadline = tokio::time::sleep(self.options.job_timeout);
            tokio::pin!(deadline);
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let heartbeat_every = Duration::from_secs(u64::from(
                (self.options.lease_seconds / 3).unsigned_abs(),
            ));
            let mut renewed = tokio::time::Instant::now();
            let outcome = loop {
                tokio::select! {
                    result = operation.join_next() => break match result {
                        Some(result) => result
                            .context(TaskSnafu {
                                stage: "document-parse-task",
                                code: ApiCode::COMMON_INTERNAL_ERROR,
                            })
                            .and_then(|result| result),
                        None => RequestSnafu {
                            stage: "document-join-task",
                            code: ApiCode::COMMON_INTERNAL_ERROR,
                        }.fail(),
                    },
                    _ = &mut deadline => break RequestSnafu {
                        stage: "document-parse-timeout",
                        code: ApiCode::unprocessable_entity(4221001),
                    }.fail(),
                    _ = tick.tick() => {
                        let snapshot = if progress.has_changed().unwrap_or(false) {
                            progress.borrow_and_update().clone()
                                .map(serde_json::to_value).transpose()
                                .context(SerializeSnafu {
                                    stage: "task-serialize-progress",
                                    code: ApiCode::COMMON_INTERNAL_ERROR,
                                })?
                        } else { None };
                        if snapshot.is_some() || renewed.elapsed() >= heartbeat_every {
                            // A blocked database query must not suspend cancellation and supervision indefinitely.
                            let renewed_lease = tokio::time::timeout(heartbeat_every,
                                Jobs::heartbeat(&self.db, &lease, self.options.lease_seconds, snapshot))
                                .await.map_err(|_elapsed| RequestSnafu {
                                    stage: "task-renew-timeout",
                                    code: ApiCode::COMMON_DATABASE_ERROR,
                                }.build())?
                                .with_context(|source| DatabaseSnafu {
                                    stage: "task-renew-lease",
                                    code: ApiCode::from(&*source),
                                })?;
                            if !renewed_lease {
                                tracing::warn!("job {} attempt {} lost its lease; discarding this attempt", id, lease.job.attempts);
                                return Ok(());
                            }
                            renewed = tokio::time::Instant::now();
                        }
                    }
                }
            };
            // Abort waiting parser tasks before publishing timeout/failure; already-running native calls retain their buffers safely.
            operation.abort_all();
            // Keep success and failure disjoint through the database completion boundary.
            let outcome = outcome.map_err(|error| {
                tracing::warn!(
                    "job {} attempt {} failed: {}",
                    id,
                    lease.job.attempts,
                    error
                );
                error.message()
            });
            // Read the final callback even when parsing finishes before the next periodic progress flush.
            let final_progress = progress
                .borrow_and_update()
                .clone()
                .map(serde_json::to_value)
                .transpose()
                .context(SerializeSnafu {
                    stage: "task-serialize-final-progress",
                    code: ApiCode::COMMON_INTERNAL_ERROR,
                })?;
            // Capture one monotonic measurement for both persistence and logs, before the atomic completion update.
            let duration = started.elapsed();
            let accepted = Jobs::finish(
                &self.db,
                &lease,
                outcome.as_deref().map_err(String::as_str),
                final_progress,
                duration,
            )
            .await
            .with_context(|source| DatabaseSnafu {
                stage: "task-finish-attempt",
                code: ApiCode::from(&*source),
            })?;
            tracing::info!(
                "finished job {} attempt {} in {} ms; accepted={}, succeeded={}",
                id,
                lease.job.attempts,
                duration.as_millis(),
                accepted,
                outcome.is_ok()
            );
            Ok(())
        }
        .inspect_err(|error| {
            tracing::warn!("worker attempt stopped; lease recovery remains available: {}", error);
        })
        .instrument(span)
        .with_current_subscriber()
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_core::Timing;
    use docparse_layout::timing::TimingStage;

    /// HTTP workers expose slow model queues at INFO without logging every short stage.
    #[test]
    fn progress_observer_logs_slow_stages() {
        let log = tempfile::NamedTempFile::new().expect("log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::INFO)
            .with_writer(log.reopen().expect("writer"))
            .finish();
        let (sender, _) = watch::channel(None);
        let observer = ProgressObserver(sender);
        tracing::subscriber::with_default(subscriber, || {
            for (stage, duration_ms) in [
                (TimingStage::FormulaQueue, 1500.0),
                (TimingStage::TsrInference, 2500.0),
                (TimingStage::FormulaDecode, 5.0),
            ] {
                observer.on_timing(Timing {
                    stage,
                    page_number: Some(2),
                    duration_ms,
                });
            }
        });
        let text = std::fs::read_to_string(log.path()).expect("logs");
        assert!(
            text.contains("FormulaQueue for page Some(2) elapsed 1500.000 ms")
        );
        assert!(
            text.contains("TsrInference for page Some(2) elapsed 2500.000 ms")
        );
        assert!(!text.contains("FormulaDecode"));
    }
}
