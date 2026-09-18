use docparse_core::pdfium_ipc::{
    Bootstrap, Command, Hello, Outcome, Reply, Request,
};
use docparse_core::{GlyphResolver, PdfiumRuntimeError, WasmBoxedFuture};
use ipc_channel::ipc::{self, IpcSender};
use std::{
    path::Path, process::Stdio, sync::Arc, thread::JoinHandle, time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Child,
    sync::{mpsc, oneshot},
};
use typed_builder::TypedBuilder;

pub(super) const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// One bounded bridge request; only this thread performs blocking IPC operations.
#[derive(TypedBuilder)]
struct BridgeCall {
    // IPC work may outlive the caller and must keep completion-counted render capacity.
    #[builder(default)]
    _page_lease: Option<docparse_common::PageLease>,
    lease: u64,
    command: Command,
    #[builder(default)]
    resolver: Option<Arc<dyn GlyphResolver>>,
    response: oneshot::Sender<Result<Outcome, PdfiumRuntimeError>>,
}

/// Owns the OS process and its bridge until both have actually terminated.
#[derive(TypedBuilder)]
pub(super) struct Process {
    pub child: Child,
    bridge: mpsc::Sender<BridgeCall>,
    #[builder(default)]
    thread: Option<JoinHandle<()>>,
    finished: oneshot::Receiver<()>,
    // The parent removes rendezvous files even when a child is killed before accept().
    bootstrap_directory: tempfile::TempDir,
}

impl Process {
    /// Spawns a matching worker and rolls back every partially initialized resource on failure.
    pub async fn start(
        binary: &Path,
        stopping: &tokio_util::sync::CancellationToken,
    ) -> Result<Self, PdfiumRuntimeError> {
        if stopping.is_cancelled() {
            return Err(PdfiumRuntimeError::Transport(
                "PDFium startup cancelled".into(),
            ));
        }
        let started = std::time::Instant::now();
        let bootstrap_directory =
            tempfile::Builder::new().prefix("dp-").tempdir()?;
        let child = tokio::process::Command::new(binary)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env("TMPDIR", bootstrap_directory.path())
            // Terminal interrupts go to the supervisor, which drains jobs and sends IPC shutdown.
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                tracing::error!(
                    "could not start PDFium worker {}: {}",
                    binary.display(),
                    error
                );
                PdfiumRuntimeError::from(error)
            })?;
        let (bridge, requests) = mpsc::channel(1);
        let (finished, completed) = oneshot::channel();
        let mut process = Self::builder()
            .child(child)
            .bridge(bridge)
            .finished(completed)
            .bootstrap_directory(bootstrap_directory)
            .build();
        let setup = async {
            let stdout = process.child.stdout.take().ok_or_else(|| {
                PdfiumRuntimeError::Transport("missing worker stdout".into())
            })?;
            // Limit the read itself so an invalid worker cannot allocate an unbounded bootstrap line.
            let mut reader = BufReader::new(stdout.take(4097));
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await?;
            if line.len() > 4096 || line.last() != Some(&b'\n') {
                return Err(PdfiumRuntimeError::Transport(
                    "invalid PDFium bootstrap length".into(),
                ));
            }
            let hello: Hello = serde_json::from_slice(&line)?;
            hello.validate()?;
            let (ready, initialized) = oneshot::channel();
            process.thread = Some(
                std::thread::Builder::new()
                    .name("docparse-pdfium-ipc".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(
                            std::panic::AssertUnwindSafe(|| {
                                Self::bridge_loop(
                                    hello.endpoint,
                                    requests,
                                    ready,
                                )
                            }),
                        );
                        match result {
                            Ok(Err(error)) => tracing::warn!(
                                "PDFium IPC bridge stopped: {}",
                                error
                            ),
                            Err(_) => {
                                tracing::error!("PDFium IPC bridge panicked")
                            }
                            Ok(Ok(())) => {}
                        }
                        let _ = finished.send(());
                    })?,
            );
            initialized.await.map_err(|_error| {
                PdfiumRuntimeError::Transport(
                    "worker handshake channel closed".into(),
                )
            })??;
            Ok::<(), PdfiumRuntimeError>(())
        };
        let result = tokio::select! {
            result = tokio::time::timeout(START_TIMEOUT, setup) => match result {
                Ok(result) => result,
                Err(_) => Err(PdfiumRuntimeError::Transport("PDFium worker startup timed out".into())),
            },
            _ = stopping.cancelled() => Err(PdfiumRuntimeError::Transport("PDFium startup cancelled".into())),
        };
        if let Err(error) = result {
            tracing::error!(
                "PDFium worker {:?} startup failed: {}",
                process.child.id(),
                error
            );
            if let Err(cleanup) = process.stop(false).await {
                tracing::error!("PDFium startup cleanup failed: {}", cleanup);
            }
            return Err(error);
        }
        tracing::info!(
            "PDFium worker {:?} ready after {:.3}s",
            process.child.id(),
            started.elapsed().as_secs_f64()
        );
        Ok(process)
    }

    /// Exchanges one command without retaining a borrow of the separately supervised Child.
    pub fn exchange(
        &self,
        lease: u64,
        command: Command,
        resolver: Option<Arc<dyn GlyphResolver>>,
        page_lease: Option<docparse_common::PageLease>,
    ) -> WasmBoxedFuture<'static, Result<Outcome, PdfiumRuntimeError>> {
        let bridge = self.bridge.clone();
        Box::pin(async move {
            let (response, result) = oneshot::channel();
            bridge
                .send(
                    BridgeCall::builder()
                        ._page_lease(page_lease)
                        .lease(lease)
                        .command(command)
                        .resolver(resolver)
                        .response(response)
                        .build(),
                )
                .await
                .map_err(|_error| {
                    PdfiumRuntimeError::Transport("PDFium bridge closed".into())
                })?;
            result.await.map_err(|_error| {
                PdfiumRuntimeError::Transport(
                    "PDFium response channel closed".into(),
                )
            })?
        })
    }

    /// Stops and reaps the child before joining its bridge, even after cancellation or bad input.
    pub async fn stop(
        mut self,
        graceful: bool,
    ) -> Result<(), PdfiumRuntimeError> {
        let pid = self.child.id();
        if graceful {
            tracing::info!(
                "requesting graceful shutdown of PDFium worker {:?}",
                pid
            );
        }
        let acknowledged = graceful
            && matches!(
                tokio::time::timeout(
                    CLOSE_TIMEOUT,
                    self.exchange(0, Command::Shutdown, None, None)
                )
                .await,
                Ok(Ok(Outcome::Closed))
            );
        // Releasing this sender wakes an idle bridge; an active receive wakes when the child exits.
        drop(self.bridge);
        if self.child.try_wait()?.is_none() {
            if !acknowledged {
                tracing::warn!(
                    "PDFium worker {:?} has no shutdown acknowledgement; forcing termination",
                    pid
                );
                self.child.start_kill()?;
            }
            match tokio::time::timeout(CLOSE_TIMEOUT, self.child.wait()).await {
                Ok(result) => {
                    result?;
                }
                Err(_deadline) => {
                    tracing::warn!(
                        "PDFium worker {:?} did not exit within {:?}; forcing termination",
                        pid,
                        CLOSE_TIMEOUT
                    );
                    self.child.start_kill()?;
                    self.child.wait().await?;
                }
            }
        }
        if let Some(thread) = self.thread.take() {
            tokio::time::timeout(CLOSE_TIMEOUT, &mut self.finished).await
                .map_err(|_error| PdfiumRuntimeError::Transport("PDFium bridge did not stop; a synchronous resolver may be stuck".into()))?
                .map_err(|_error| PdfiumRuntimeError::Transport("PDFium bridge completion channel closed".into()))?;
            thread
                .join()
                .map_err(|_error| PdfiumRuntimeError::WorkerPanicked)?;
        }
        self.bootstrap_directory.close()?;
        tracing::info!(
            "PDFium worker {:?} reaped and IPC bridge joined (shutdown acknowledged={})",
            pid,
            acknowledged
        );
        Ok(())
    }

    /// Connects once, then services bounded requests and synchronous reverse glyph calls.
    fn bridge_loop(
        endpoint: String,
        mut requests: mpsc::Receiver<BridgeCall>,
        ready: oneshot::Sender<Result<(), PdfiumRuntimeError>>,
    ) -> Result<(), PdfiumRuntimeError> {
        let connected = (|| {
            let (commands, receiver) = ipc::channel::<Request>()?;
            let (responses, replies) = ipc::channel::<Reply>()?;
            let connector = IpcSender::<Bootstrap>::connect(endpoint)?;
            connector.send(Bootstrap {
                commands: receiver,
                responses,
            })?;
            drop(connector);
            let initial = replies.recv()?;
            if initial.lease != 0
                || initial.id != 0
                || !matches!(initial.outcome, Outcome::Ready)
            {
                return Err(PdfiumRuntimeError::Transport(
                    "expected PDFium Ready response".into(),
                ));
            }
            Ok::<_, PdfiumRuntimeError>((commands, replies))
        })();
        let (commands, replies) = match connected {
            Ok(channels) => {
                if ready.send(Ok(())).is_err() {
                    return Ok(());
                }
                channels
            }
            Err(error) => {
                let _ = ready.send(Err(error));
                return Ok(());
            }
        };
        let mut id = 0_u64;
        while let Some(call) = requests.blocking_recv() {
            id = id.checked_add(1).ok_or_else(|| {
                PdfiumRuntimeError::Transport(
                    "PDFium request counter exhausted".into(),
                )
            })?;
            let result = (|| {
                commands.send(Request {
                    lease: call.lease,
                    id,
                    command: call.command,
                })?;
                loop {
                    let reply = replies.recv()?;
                    if reply.lease != call.lease || reply.id != id {
                        return Err(PdfiumRuntimeError::Transport(
                            "stale PDFium response".into(),
                        ));
                    }
                    match reply.outcome {
                        Outcome::Glyph(segments) => {
                            let resolver =
                                call.resolver.as_ref().ok_or_else(|| {
                                    PdfiumRuntimeError::Transport(
                                        "unexpected glyph callback".into(),
                                    )
                                })?;
                            let value = std::panic::catch_unwind(
                                std::panic::AssertUnwindSafe(|| {
                                    resolver.resolve(&segments)
                                }),
                            )
                            .map_err(|_error| {
                                PdfiumRuntimeError::Transport(
                                    "glyph resolver panicked".into(),
                                )
                            })?;
                            commands.send(Request {
                                lease: call.lease,
                                id,
                                command: Command::GlyphReply(value),
                            })?;
                        }
                        Outcome::Failure { message, fatal } => {
                            return Err(if fatal {
                                PdfiumRuntimeError::Transport(message)
                            } else {
                                PdfiumRuntimeError::RemotePage(message)
                            });
                        }
                        outcome => return Ok(outcome),
                    }
                }
            })();
            let fatal =
                result.as_ref().is_err_and(PdfiumRuntimeError::is_fatal);
            let _ = call.response.send(result);
            if fatal {
                break;
            }
        }
        Ok(())
    }
}
