use crate::{
    code::ApiCode,
    error::{
        ApiResult, CpuTaskSnafu, DatabaseSnafu, ParseSnafu, RequestSnafu,
        SerializeSnafu, StorageSnafu, TaskSnafu,
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

/// Durable attempt supervision covers parsing, result publication, and cancellation cleanup.
#[derive(Clone, TypedBuilder)]
pub struct WorkerOptions {
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
        if !(3..=86400).contains(&self.lease_seconds)
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
    // Reserve before claiming so parsed results and publication never become an unbounded second queue.
    #[builder(default = Arc::new(tokio::sync::Semaphore::new(parser.document_capacity())))]
    admission: Arc<tokio::sync::Semaphore>,
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

    /// Records every completed stage as a metric and slow ones at TRACE.
    ///
    /// The stage label is bounded by the stage enum, so production keeps per-stage visibility
    /// without flooding logs; the TRACE line stays for focused local debugging.
    fn on_timing(&self, timing: docparse_core::Timing) {
        metrics::histogram!(
            "docparse_parse_stage_seconds",
            "stage" => format!("{:?}", timing.stage)
        )
        .record(timing.duration_ms / 1000.0);
        // Backpressure commonly exceeds this threshold; require TRACE to avoid flooding production logs.
        if timing.duration_ms >= 1000.0 {
            tracing::trace!(
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
    pub async fn run(
        self,
        pool: Arc<crate::pdfium_pool::PdfiumPool>,
        shutdown: CancellationToken,
    ) -> ApiResult<()> {
        self.options.validate()?;
        let mut tasks = JoinSet::new();
        let mut next_poll = tokio::time::Instant::now();
        loop {
            if shutdown.is_cancelled() && tasks.is_empty() {
                return Ok(());
            }
            tokio::select! {
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(_)) = result { tracing::error!("worker task panicked or was cancelled; its lease will expire"); }
                }
                reservation = async {
                    tokio::time::sleep_until(next_poll).await;
                    let resources = docparse_common::ResourceLease::acquire(Arc::clone(&self.admission)).await
                        .context(CpuTaskSnafu { stage: "worker-admit-document", code: ApiCode::COMMON_INTERNAL_ERROR })?;
                    let reservation = pool.reserve().await.map_err(|error| docparse_core::DocParseError::Runtime { source: Box::new(error) })
                        .context(ParseSnafu { stage: "worker-reserve-pdfium", code: ApiCode::service_unavailable(5031003) })?;
                    Ok::<_, crate::error::ApiError>((resources, reservation))
                }, if !shutdown.is_cancelled() => {
                    let (resources, reservation) = reservation?;
                    // Both the complete document lifetime and an idle renderer must have capacity before claiming.
                    let claimed = tokio::select! {
                        _ = shutdown.cancelled() => { drop(reservation); continue; }
                        result = Jobs::claim(&self.db, self.options.lease_seconds, self.options.max_attempts) => result,
                    };
                    match claimed {
                        Ok(Some(lease)) => {
                            let worker = self.clone();
                            tasks.spawn(async move { worker.process_with_reservation(lease, Some(reservation), Some(resources)).await }.with_current_subscriber());
                            next_poll = tokio::time::Instant::now();
                        }
                        outcome => {
                            drop(reservation);
                            if let Err(error) = outcome { tracing::warn!("task claim failed; retrying: {}", error); }
                            next_poll = tokio::time::Instant::now() + Duration::from_millis(500);
                        }
                    }
                }
                _ = shutdown.cancelled(), if !shutdown.is_cancelled() => {
                    tracing::info!("worker is draining {} in-flight jobs", tasks.len());
                }
            }
        }
    }

    /// Parses one fenced attempt while renewing its lease through PDF, inference, and result publication.
    pub async fn process(&self, lease: Lease) -> ApiResult<()> {
        // Explicitly claimed attempts must renew their lease even while waiting for document admission.
        self.process_with_reservation(lease, None, None).await
    }

    /// Supervises a claimed job using its reserved process, while retaining the direct attempt API for callers.
    async fn process_with_reservation(
        &self,
        lease: Lease,
        reservation: Option<crate::pdfium_pool::PdfiumReservation>,
        mut resources: Option<docparse_common::ResourceLease>,
    ) -> ApiResult<()> {
        // Recreate a local span from durable fields for every attempt; no process-local Span ID is persisted.
        let span = tracing::info_span!(target: crate::logging::CONTEXT_TARGET, parent: None, "pdf_parse",
            job_id = %lease.job.id, pdf_hash = %lease.job.input_hash, attempt = lease.job.attempts);
        async move {
            // Construct business APICODEs in each context so persisted messages retain the same contract as HTTP responses.
            self.options.validate()?;
            let id = lease.job.id;
            tracing::info!("starting job {} attempt {}", id, lease.job.attempts);
            let _active = docparse_common::telemetry::Activity::new("docparse_job_attempts_active", ("scope", "local"), 1.0);
            let started = tokio::time::Instant::now();
            let (sender, mut progress) = watch::channel(None);
            let observer = ProgressObserver(sender);
            // Own the parse in a separate task: synchronous document work must not stop the lease watchdog.
            // JoinSet aborts this owned task if supervision is cancelled or loses its database lease.
            let worker = self.clone();
            let input_hash = lease.job.input_hash.clone();
            let token = lease.token;
            let name = format!("{id}-{token}.json");
            // The attempt prefix ties assets to deletion/recovery without trusting paths from stored JSON.
            let figure_assets = self.parser.figure_assets(self.storage.root().to_path_buf(), format!("{name}.figures-"));
            let assets = Arc::clone(&figure_assets);
            let mut operation = JoinSet::new();
            let operation_resources = resources.clone();
            let admission = Arc::clone(&self.admission);
            operation.spawn(async move {
                let operation_resources = match operation_resources {
                    Some(resources) => resources,
                    None => docparse_common::ResourceLease::acquire(admission).await
                        .context(CpuTaskSnafu { stage: "worker-admit-document", code: ApiCode::COMMON_INTERNAL_ERROR })?,
                };
                let outcome = operation_resources.scope(worker.parse_and_publish(
                    &input_hash, &name, reservation, assets, &observer,
                )).await.map(|()| name);
                // Transfer ownership back to supervision before the durable completion acknowledgement.
                Ok::<_, crate::error::ApiError>((outcome, operation_resources))
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
                            .and_then(|result| result)
                            .and_then(|(outcome, owner)| { resources = Some(owner); outcome }),
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
            let final_progress = progress.borrow_and_update().clone();
            let completion = self.finish_attempt(&lease, outcome, final_progress, started.elapsed(), figure_assets);
            match resources {
                Some(owner) => owner.scope(completion).await,
                None => completion.await,
            }

        }
        .inspect_err(|error| {
            tracing::warn!("worker attempt stopped; lease recovery remains available: {}", error);
        })
        .instrument(span)
        .with_current_subscriber()
        .await
    }

    /// Parses one admitted attempt and publishes its canonical result before durable completion is acknowledged.
    async fn parse_and_publish(
        &self,
        input_hash: &str,
        name: &str,
        reservation: Option<crate::pdfium_pool::PdfiumReservation>,
        assets: Arc<docparse_core::FigureAssets>,
        observer: &ProgressObserver,
    ) -> ApiResult<()> {
        let input = self.storage.path(&format!("{input_hash}.pdf"))?;
        let options = ParseOptions::builder()
            .observer(Some(observer))
            .figure_assets(Some(Arc::clone(&assets)))
            .build();
        let parsing = docparse_common::telemetry::Timer::new(
            "docparse_job_parse_seconds",
            "scope",
            "local",
        );
        let result = if let Some(reservation) = reservation {
            observer.on_progress(ParseProgress::Opening);
            let session = reservation
                .open(
                    docparse_core::PdfInput::Path(input),
                    docparse_common::timing::Timings::default(),
                )
                .await
                .map_err(|error| docparse_core::DocParseError::Runtime {
                    source: Box::new(error),
                })
                .context(ParseSnafu {
                    stage: "document-open-pdf",
                    code: ApiCode::unprocessable_entity(4221001),
                })?;
            self.parser
                .parse_session_with_options(session, options)
                .await
        } else {
            self.parser.parse_path_with_options(input, options).await
        }
        .context(ParseSnafu {
            stage: "document-parse-pdf",
            code: ApiCode::unprocessable_entity(4221001),
        })?;
        drop(parsing);
        metrics::counter!("docparse_pages_parsed_total")
            .increment(result.pages.len() as u64);
        self.publish_result(result, name, assets).await
    }

    /// Streams serialization on the CPU pool and retains publication ownership through atomic storage writes.
    async fn publish_result(
        &self,
        result: docparse_core::DocumentResult,
        name: &str,
        assets: Arc<docparse_core::FigureAssets>,
    ) -> ApiResult<()> {
        let mut temporary = self.storage.temporary().await?;
        let publishing = docparse_common::telemetry::Timer::new(
            "docparse_job_publish_seconds",
            "scope",
            "local",
        );
        let output = self.output.clone();
        // run_cpu restores tracing and admission once; the writer only owns serialization and its resources.
        let publication = docparse_common::run_cpu(move || -> ApiResult<_> {
            let _assets = assets;
            let mut writer = std::io::BufWriter::with_capacity(
                256 * 1024,
                temporary.as_file_mut(),
            );
            ApiResponse::data(docparse_core::JsonRenderer::view_with_config(
                &result, &output,
            ))
            .write(&mut writer)
            .context(SerializeSnafu {
                stage: "result-serialize-json",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            })?;
            writer.flush().context(StorageSnafu {
                stage: "result-flush-file",
                code: ApiCode::service_unavailable(5031003),
            })?;
            drop(writer);
            Ok((temporary, publishing))
        })
        .await
        .context(CpuTaskSnafu {
            stage: "result-write-task",
            code: ApiCode::COMMON_INTERNAL_ERROR,
        })??;
        // Attempt-specific names keep abandoned writers from replacing a successor's result.
        self.storage.publish(publication, name).await
    }

    /// Fences final state in PostgreSQL and commits figure ownership only after the result is accepted.
    async fn finish_attempt(
        &self,
        lease: &Lease,
        outcome: ApiResult<String>,
        progress: Option<ParseProgress>,
        duration: Duration,
        figure_assets: Arc<docparse_core::FigureAssets>,
    ) -> ApiResult<()> {
        let id = lease.job.id;
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
        let final_progress = progress
            .map(serde_json::to_value)
            .transpose()
            .context(SerializeSnafu {
                stage: "task-serialize-final-progress",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            })?;
        // Cancellation while awaiting the database can hide a successful commit; recovery owns that uncertainty.
        if outcome.is_ok() {
            figure_assets.keep(false);
        }
        let completion = Jobs::finish(
            &self.db,
            lease,
            outcome.as_deref().map_err(String::as_str),
            final_progress,
            duration,
        )
        .await;
        // Preserve ambiguous acknowledgements for recovery; never remove possibly committed assets.
        if outcome.is_ok() && matches!(&completion, Ok(true)) {
            let assets = Arc::clone(&figure_assets);
            docparse_common::run_blocking(move || assets.keep(true))
                .await
                .context(CpuTaskSnafu {
                    stage: "result-keep-figures",
                    code: ApiCode::COMMON_INTERNAL_ERROR,
                })?;
        }
        let accepted = completion.with_context(|source| DatabaseSnafu {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_common::timing::TimingStage;
    use docparse_core::Timing;

    /// Slow model queues remain observable at TRACE without recording short stages.
    #[test]
    fn progress_observer_logs_slow_stages() {
        let log = tempfile::NamedTempFile::new().expect("log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            // Match the production observer's TRACE policy instead of expecting INFO output.
            .with_max_level(tracing::Level::TRACE)
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
