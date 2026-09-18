//! Server-owned PDFium processes with document-affine leases and a strict live-process limit.
mod process;

use docparse_common::timing::{TimingStage, Timings};
use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_core::pdfium_ipc::{Command, Outcome, Source, WORKER_BINARY};
use docparse_core::{
    GlyphResolver, PdfInput, PdfiumProvider, PdfiumRuntimeError, PdfiumSession,
    PreScannedPage, RenderedPage, WasmBoxedFuture,
};
use process::{CLOSE_TIMEOUT, Process};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

/// One bounded application request, independently cancellable through its owning lease.
#[derive(TypedBuilder)]
struct Call {
    #[builder(default = docparse_common::PageLease::current())]
    page_lease: Option<docparse_common::PageLease>,
    command: Command,
    #[builder(default)]
    resolver: Option<Arc<dyn GlyphResolver>>,
    response: oneshot::Sender<Result<Outcome, PdfiumRuntimeError>>,
}

/// Dropping a lease always notifies its process supervisor, including cancellation during Open.
struct Lease {
    commands: mpsc::Sender<Call>,
    cancelled: CancellationToken,
    admission: Mutex<()>,
}
impl Drop for Lease {
    /// Never returns an unconfirmed document to the idle pool.
    fn drop(&mut self) {
        self.cancelled.cancel();
    }
}
impl Lease {
    /// Waits asynchronously while the bounded bridge performs the actual IPC.
    async fn request(
        &self,
        command: Command,
        resolver: Option<Arc<dyn GlyphResolver>>,
    ) -> Result<Outcome, PdfiumRuntimeError> {
        // Cancelling a queued call leaves the active command alone; admitted calls own cancellation.
        let _admission = tokio::select! {
            _ = self.cancelled.cancelled() => return Err(PdfiumRuntimeError::Transport("PDFium lease cancelled".into())),
            guard = self.admission.lock() => guard,
        };
        let uncertain = self.cancelled.clone().drop_guard();
        let (response, reply) = oneshot::channel();
        let result = tokio::select! {
            _ = self.cancelled.cancelled() => Err(PdfiumRuntimeError::Transport("PDFium lease cancelled".into())),
            result = async {
                self.commands.send(Call::builder().command(command).resolver(resolver).response(response).build()).await
                    .map_err(|_error| PdfiumRuntimeError::Transport("PDFium lease stopped".into()))?;
                reply.await.map_err(|_error| PdfiumRuntimeError::Transport("PDFium lease response closed".into()))?
            } => result,
        };
        if !result.as_ref().is_err_and(PdfiumRuntimeError::is_fatal) {
            uncertain.disarm();
        }
        result
    }
}

/// Every parser clone in one server shares these N process slots.
#[derive(TypedBuilder)]
pub struct PdfiumPool {
    // This is process-handle inventory, not a work queue; stale tokens must not crowd out healthy returns.
    returned: mpsc::UnboundedSender<Lease>,
    available: Mutex<mpsc::UnboundedReceiver<Lease>>,
    stopping: CancellationToken,
    finished: watch::Receiver<Option<Result<(), String>>>,
    _capacity: docparse_common::telemetry::Activity,
}

impl Drop for PdfiumPool {
    /// Supervisors own their processes independently and finish cleanup after the last client is dropped.
    fn drop(&mut self) {
        self.stopping.cancel();
    }
}

impl PdfiumPool {
    /// Starts workers beside the server executable; cancellation still leaves supervised cleanup running.
    pub async fn start(
        max_processes: usize,
        server_executable: &Path,
    ) -> Result<Arc<Self>, PdfiumRuntimeError> {
        if max_processes == 0
            || max_processes > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(PdfiumRuntimeError::Transport(
                "PDFium process limit must be positive and fit the runtime semaphore".into(),
            ));
        }
        let binary = server_executable
            .parent()
            .ok_or_else(|| {
                PdfiumRuntimeError::Transport(
                    "server executable has no parent directory".into(),
                )
            })?
            .join(format!("{WORKER_BINARY}{}", std::env::consts::EXE_SUFFIX));
        // Supervisors still enforce max_processes; expired idle handles may coexist with their replacements.
        let (available, leases) = mpsc::unbounded_channel();
        let stopping = CancellationToken::new();
        let (completed, finished) = watch::channel(None);
        let pool = Arc::new(
            Self::builder()
                ._capacity(docparse_common::telemetry::Activity::new(
                    "docparse_pdfium_workers_configured",
                    ("pool", "render"),
                    max_processes as f64,
                ))
                .returned(available.clone())
                .available(Mutex::new(leases))
                .stopping(stopping.clone())
                .finished(finished)
                .build(),
        );
        let (ready, initialized) = oneshot::channel();
        // This task owns all processes even if the caller drops the startup future.
        tokio::spawn(async move {
            let mut processes = Vec::new();
            if let Err(error) = processes.try_reserve_exact(max_processes) {
                let error =
                    format!("cannot reserve PDFium process slots: {error}");
                tracing::error!("{}", error);
                stopping.cancel();
                let _ = ready.send(Err(error.clone()));
                completed.send_replace(Some(Err(error)));
                return;
            }
            let mut failure = None;
            for slot in 0..max_processes {
                match Process::start(&binary, &stopping).await {
                    Ok(process) => {
                        tracing::info!(
                            "PDFium slot {} started PID {:?}",
                            slot,
                            process.child.id()
                        );
                        processes.push(process);
                    }
                    Err(error) => {
                        failure = Some(error.to_string());
                        break;
                    }
                }
            }
            if let Some(error) = failure {
                stopping.cancel();
                for process in processes {
                    if let Err(cleanup) = process.stop(false).await {
                        tracing::error!(
                            "PDFium startup rollback failed: {}",
                            cleanup
                        );
                    }
                }
                let _ = ready.send(Err(error.clone()));
                completed.send_replace(Some(Err(error)));
                return;
            }
            let mut supervisors = tokio::task::JoinSet::new();
            for (slot, process) in processes.into_iter().enumerate() {
                supervisors.spawn(Self::supervise(
                    slot,
                    process,
                    binary.clone(),
                    available.clone(),
                    stopping.clone(),
                ));
            }
            drop(available);
            if ready.send(Ok(())).is_err() {
                stopping.cancel();
            }
            let mut result = Ok(());
            while let Some(outcome) = supervisors.join_next().await {
                let error = match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error.to_string()),
                    Err(error) => {
                        Some(format!("PDFium supervisor failed: {error}"))
                    }
                };
                if let Some(error) = error {
                    tracing::error!("PDFium pool is unavailable: {}", error);
                    stopping.cancel();
                    if result.is_ok() {
                        result = Err(error);
                    }
                }
            }
            completed.send_replace(Some(result));
        });
        initialized
            .await
            .map_err(|_error| {
                PdfiumRuntimeError::Transport(
                    "PDFium pool startup task stopped".into(),
                )
            })?
            .map_err(PdfiumRuntimeError::Transport)?;
        Ok(pool)
    }

    /// Reserves an idle process before claiming a durable job; unused reservations return without restarting it.
    pub async fn reserve(
        &self,
    ) -> Result<PdfiumReservation, PdfiumRuntimeError> {
        let lease = loop {
            let lease = tokio::select! {
                biased;
                _ = self.stopping.cancelled() => return Err(PdfiumRuntimeError::Transport("PDFium pool is closed".into())),
                lease = async { self.available.lock().await.recv().await } => lease,
            }.ok_or_else(|| PdfiumRuntimeError::Transport("PDFium pool has no active slots".into()))?;
            if !lease.cancelled.is_cancelled() && !lease.commands.is_closed() {
                break lease;
            }
        };
        Ok(PdfiumReservation {
            lease: Some(lease),
            returned: self.returned.clone(),
            stopping: self.stopping.clone(),
        })
    }

    /// Stops accepting documents and waits for the same cleanup result on every call.
    pub async fn shutdown(&self) -> Result<(), PdfiumRuntimeError> {
        self.stopping.cancel();
        let mut finished = self.finished.clone();
        let result = finished
            .wait_for(Option::is_some)
            .await
            .map_err(|_error| {
                PdfiumRuntimeError::Transport(
                    "PDFium pool completion channel closed".into(),
                )
            })?
            .clone()
            .ok_or_else(|| {
                PdfiumRuntimeError::Transport(
                    "missing PDFium pool completion".into(),
                )
            })?;
        result.map_err(PdfiumRuntimeError::Transport)
    }

    /// Wakes the server when a pool failure requires stopping durable job admission.
    pub async fn stopped(&self) {
        self.stopping.cancelled().await;
    }

    /// Supervises one fixed slot; replacement is allowed only after the old process and bridge are reaped.
    async fn supervise(
        slot: usize,
        mut process: Process,
        binary: PathBuf,
        available: mpsc::UnboundedSender<Lease>,
        stopping: CancellationToken,
    ) -> Result<(), PdfiumRuntimeError> {
        let mut alive = Some(docparse_common::telemetry::Activity::new(
            "docparse_pdfium_workers_alive",
            ("pool", "render"),
            1.0,
        ));
        let mut lease_id = 0_u64;
        let mut replacement = false;
        loop {
            lease_id = lease_id.checked_add(1).ok_or_else(|| {
                PdfiumRuntimeError::Transport(
                    "PDFium lease counter exhausted".into(),
                )
            })?;
            let (commands, mut requests) = mpsc::channel::<Call>(1);
            let cancelled = CancellationToken::new();
            let lease = Lease {
                commands,
                cancelled: cancelled.clone(),
                admission: Mutex::new(()),
            };
            // Invalid documents and deliberate cancellation do not consume the crash-restart budget.
            let mut failed = false;
            let offered = tokio::select! {
                biased;
                _ = stopping.cancelled() => false,
                status = process.child.wait() => {
                    tracing::warn!("PDFium slot {} exited before admission: {:?}", slot, status);
                    failed = true;
                    false
                }
                result = async { available.send(lease) } => result.is_ok(),
            };
            let mut active_page = None;
            let mut occupied = None;
            let mut completed = false;
            let mut rejected = false;
            if offered {
                tracing::debug!(
                    "PDFium slot {} PID {:?} offered lease {}",
                    slot,
                    process.child.id(),
                    lease_id
                );
                loop {
                    let call = tokio::select! {
                        biased;
                        _ = stopping.cancelled() => None,
                        _ = cancelled.cancelled() => None,
                        status = process.child.wait() => {
                            tracing::warn!("PDFium slot {} lease {} exited: {:?}", slot, lease_id, status);
                            failed = true;
                            None
                        }
                        call = requests.recv() => call,
                    };
                    let Some(call) = call else {
                        break;
                    };
                    let closing = matches!(call.command, Command::Close);
                    let opening = matches!(call.command, Command::Open(_));
                    if opening && occupied.is_none() {
                        occupied =
                            Some(docparse_common::telemetry::Activity::new(
                                "docparse_pdfium_documents_active",
                                ("pool", "render"),
                                1.0,
                            ));
                    }
                    // The supervisor scope includes the real IPC, even if the caller abandons its reply.
                    let operation = match &call.command {
                        Command::Open(_) => "open",
                        Command::Close => "close",
                        Command::Render { .. } => "render",
                        _ => "extract",
                    };
                    let measured = docparse_common::telemetry::Timer::new(
                        "docparse_pdfium_operation_seconds",
                        "operation",
                        operation,
                    );
                    active_page = call.page_lease.clone();
                    let exchange = process.exchange(
                        lease_id,
                        call.command,
                        call.resolver,
                        call.page_lease,
                    );
                    let outcome = tokio::select! {
                        biased;
                        _ = stopping.cancelled() => Err(PdfiumRuntimeError::Transport("PDFium pool stopped".into())),
                        _ = cancelled.cancelled() => Err(PdfiumRuntimeError::Transport("PDFium document cancelled".into())),
                        status = process.child.wait() => {
                            failed = true;
                            Err(PdfiumRuntimeError::Transport(format!("PDFium worker exited: {status:?}")))
                        },
                        outcome = async {
                            if closing {
                                tokio::time::timeout(CLOSE_TIMEOUT, exchange).await
                                    .map_err(|_error| PdfiumRuntimeError::Transport("PDFium Close timed out".into()))?
                            } else { exchange.await }
                        } => {
                            failed = outcome.as_ref().is_err_and(PdfiumRuntimeError::is_fatal);
                            outcome
                        },
                    };
                    drop(measured);
                    rejected = opening
                        && matches!(
                            &outcome,
                            Err(PdfiumRuntimeError::RemotePage(_))
                        );
                    completed =
                        closing && matches!(outcome, Ok(Outcome::Closed));
                    let fatal = outcome
                        .as_ref()
                        .is_err_and(PdfiumRuntimeError::is_fatal);
                    if let Err(error) = &outcome {
                        tracing::warn!(
                            "PDFium slot {} lease {} request failed: {}",
                            slot,
                            lease_id,
                            error
                        );
                    }
                    let acknowledged = outcome.is_ok()
                        || outcome
                            .as_ref()
                            .is_err_and(|error| !error.is_fatal());
                    let _ = call.response.send(outcome);
                    if acknowledged {
                        // The IPC operation has acknowledged completion; the reply or caller owns any remaining image.
                        active_page.take();
                    }
                    if completed || rejected || fatal {
                        break;
                    }
                }
            }
            // Known-clean replies must remain observable even if their receiver polls later.
            if !completed && !rejected {
                cancelled.cancel();
            }
            drop(requests);
            if (completed || rejected) && !stopping.is_cancelled() {
                if completed {
                    replacement = false;
                }
                tracing::debug!(
                    "PDFium slot {} released lease {} (completed={})",
                    slot,
                    lease_id,
                    completed
                );
                continue;
            }
            // Cancellation requests a bounded graceful exit even while a document is open.
            // Broken transports and crashed children cannot acknowledge shutdown.
            process.stop(!failed).await?;
            drop(alive.take());
            drop(occupied.take());
            drop(active_page);
            if stopping.is_cancelled() || available.is_closed() {
                return Ok(());
            }
            if replacement && failed {
                return Err(PdfiumRuntimeError::Transport(format!(
                    "PDFium slot {slot} failed before its replacement completed a document"
                )));
            }
            tracing::warn!(
                "replacing PDFium slot {} after confirmed process and bridge cleanup",
                slot
            );
            process = match Process::start(&binary, &stopping).await {
                Ok(process) => process,
                Err(_cancelled) if stopping.is_cancelled() => return Ok(()),
                Err(error) => return Err(error),
            };
            alive = Some(docparse_common::telemetry::Activity::new(
                "docparse_pdfium_workers_alive",
                ("pool", "render"),
                1.0,
            ));
            metrics::counter!("docparse_pdfium_restarts_total").increment(1);
            // Intentional cancellation neither consumes nor restores an existing crash budget.
            replacement |= failed;
        }
    }
}

impl PdfiumProvider for PdfiumPool {
    /// Measures admission separately, then opens the document in the leased process.
    fn open<'a>(
        &'a self,
        input: PdfInput,
        _limits: &'a RuntimeConfig,
        timings: Timings,
    ) -> WasmBoxedFuture<'a, Result<Box<dyn PdfiumSession>, PdfiumRuntimeError>>
    {
        Box::pin(async move {
            let queued = timings.start(TimingStage::PdfiumQueue);
            let reservation = self.reserve().await?;
            drop(queued);
            reservation.open(input, timings).await
        })
    }
}

/// An idle process reservation that has not yet opened a document.
pub struct PdfiumReservation {
    lease: Option<Lease>,
    returned: mpsc::UnboundedSender<Lease>,
    stopping: CancellationToken,
}
impl Drop for PdfiumReservation {
    /// Returns known-idle processes directly instead of triggering document cancellation and process replacement.
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            if self.stopping.is_cancelled()
                || lease.cancelled.is_cancelled()
                || lease.commands.is_closed()
            {
                return;
            }
            if let Err(error) = self.returned.send(lease) {
                tracing::warn!(
                    "could not return idle PDFium reservation: {}",
                    error
                );
            }
        }
    }
}
impl PdfiumReservation {
    /// Opens on this exact reservation, transferring cancellation ownership only once a document is attempted.
    pub async fn open(
        mut self,
        input: PdfInput,
        timings: Timings,
    ) -> Result<Box<dyn PdfiumSession>, PdfiumRuntimeError> {
        let _opening = timings.start(TimingStage::PdfOpen);
        let source = Source::try_from(input)?;
        let lease = self.lease.take().expect("unconsumed PDFium reservation");
        let outcome = lease.request(Command::Open(source), None).await?;
        let Outcome::Opened(pages) = outcome else {
            return Err(PdfiumRuntimeError::Transport(
                "expected PDFium Opened response".into(),
            ));
        };
        if pages == 0 {
            return Err(PdfiumRuntimeError::Transport(
                "PDFium returned zero pages".into(),
            ));
        }
        Ok(Box::new(RemoteSession { lease, pages }))
    }
}

/// An owned document lease; dropping the parser future notifies the process supervisor.
struct RemoteSession {
    lease: Lease,
    pages: u32,
}
impl PdfiumSession for RemoteSession {
    /// Returns the page count validated during the Open handshake.
    fn page_count(&self) -> u32 {
        self.pages
    }
    /// Validates owned page facts after their JSON representation crosses IPC.
    fn pre_scan_page(
        &self,
        page_number: u32,
        resolver: Option<Arc<dyn GlyphResolver>>,
    ) -> WasmBoxedFuture<'_, Result<PreScannedPage, PdfiumRuntimeError>> {
        Box::pin(async move {
            if !(1..=self.pages).contains(&page_number) {
                return Err(PdfiumRuntimeError::InvalidPage {
                    page_number,
                    page_count: self.pages,
                });
            }
            let command = Command::PreScan {
                page_number,
                resolve_glyphs: resolver.is_some(),
            };
            let decoded = PreScannedPage::try_from(
                self.lease.request(command, resolver).await?,
            );
            if decoded.is_err() {
                self.lease.cancelled.cancel();
            }
            let page = decoded?;
            if page.extracted.page_number != page_number
                || !page.extracted.width.is_finite()
                || !page.extracted.height.is_finite()
                || page.extracted.width <= 0.0
                || page.extracted.height <= 0.0
            {
                self.lease.cancelled.cancel();
                return Err(PdfiumRuntimeError::Transport(
                    "invalid PDFium scan geometry or page identity".into(),
                ));
            }
            Ok(page)
        })
    }
    /// Reconstructs validated image geometry from immutable shared pixels.
    fn render_page<'a>(
        &'a self,
        page_number: u32,
        config: &'a RenderConfig,
    ) -> WasmBoxedFuture<'a, Result<RenderedPage, PdfiumRuntimeError>> {
        Box::pin(async move {
            if !(1..=self.pages).contains(&page_number) {
                return Err(PdfiumRuntimeError::InvalidPage {
                    page_number,
                    page_count: self.pages,
                });
            }
            let result = self
                .lease
                .request(
                    Command::Render {
                        page_number,
                        config: config.clone(),
                    },
                    None,
                )
                .await?;
            match result {
                Outcome::Rendered(raster)
                    if raster.page_number == page_number =>
                {
                    let decoded = RenderedPage::try_from(raster);
                    if decoded.is_err() {
                        self.lease.cancelled.cancel();
                    }
                    decoded
                }
                _ => {
                    self.lease.cancelled.cancel();
                    Err(PdfiumRuntimeError::Transport(
                        "invalid PDFium raster response".into(),
                    ))
                }
            }
        })
    }
    /// Requires the worker to release its document before acknowledging the returned lease.
    fn close(
        self: Box<Self>,
    ) -> WasmBoxedFuture<'static, Result<(), PdfiumRuntimeError>> {
        Box::pin(async move {
            match self.lease.request(Command::Close, None).await? {
                Outcome::Closed => Ok(()),
                _ => Err(PdfiumRuntimeError::Transport(
                    "expected PDFium Close acknowledgement".into(),
                )),
            }
        })
    }
}
