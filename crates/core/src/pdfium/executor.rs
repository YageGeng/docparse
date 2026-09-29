use super::{PdfInput, PdfiumWorker};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ::pdfium::{Document, Library};
use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_layout::{
    AffineTransform, Bbox, PageImage, PageImageInput, PageRotation,
    PageTransform, PageTransformInput, PixelFormat,
};
use tokio::sync::{mpsc, oneshot};
use typed_builder::TypedBuilder;

use crate::ExtractedPage;
use crate::extract::text::extract_page_text_items;

/// Upper bound on one PDFium worker operation.
///
/// A wedged FFI call must fail the page instead of parking the parse forever; the worker thread
/// itself cannot be interrupted, so it is left to finish and dropped with the executor.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Graceful shutdown deadline; a healthy worker answers immediately, a wedged one never does.
const CLOSE_DEADLINE: Duration = Duration::from_secs(5);

/// Fully owned rendered page returned across the PDFium actor boundary.
#[derive(Debug, Clone, TypedBuilder)]
pub struct RenderedPage {
    pub page_number: u32,
    pub image: Arc<PageImage>,
    pub transform: PageTransform,
    /// Image files travel with the bounded render delivery, never the document-wide pre-scan.
    #[builder(default, setter(skip))]
    pub(crate) embedded_images: Vec<crate::figure::EmbeddedImage>,
}

impl RenderedPage {
    /// Converts viewport bounds to outward-rounded, clipped pixels for both figures and formulas.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    pub(crate) fn crop_bounds(&self, bbox: Bbox) -> Result<[u32; 4], String> {
        if self.transform.render_size()
            != (self.image.width(), self.image.height())
        {
            return Err("page raster does not match its transform".to_owned());
        }
        let start =
            self.transform
                .viewport_to_rendered(docparse_layout::Point::new(
                    bbox.left, bbox.top,
                ));
        let end =
            self.transform
                .viewport_to_rendered(docparse_layout::Point::new(
                    bbox.right,
                    bbox.bottom,
                ));
        if ![start.x, start.y, end.x, end.y]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err("crop coordinates must be finite".to_owned());
        }
        let bounds = [
            start.x.floor().clamp(0.0, f64::from(self.image.width())) as u32,
            start.y.floor().clamp(0.0, f64::from(self.image.height())) as u32,
            end.x.ceil().clamp(0.0, f64::from(self.image.width())) as u32,
            end.y.ceil().clamp(0.0, f64::from(self.image.height())) as u32,
        ];
        if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
            return Err("crop is outside the page".to_owned());
        }
        Ok(bounds)
    }

    /// Copies checked pixel rows once; callers retain their own expansion and encoding policies.
    pub(crate) fn crop_pixels(
        &self,
        [left, top, right, bottom]: [u32; 4],
    ) -> Result<image::RgbImage, String> {
        let image = &self.image;
        if left >= right
            || top >= bottom
            || right > image.width()
            || bottom > image.height()
        {
            return Err("crop is outside the page".to_owned());
        }
        // Validated PageImage dimensions bound every offset and allocation below.
        let width = right - left;
        let height = bottom - top;
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(3))
            .ok_or_else(|| "crop is too large".to_owned())?;
        let capacity = usize::try_from(height)
            .ok()
            .and_then(|height| row_bytes.checked_mul(height))
            .ok_or_else(|| "crop is too large".to_owned())?;
        let mut pixels = Vec::with_capacity(capacity);
        for y in top..bottom {
            let offset = (y as usize)
                .checked_mul(image.width() as usize)
                .and_then(|offset| offset.checked_add(left as usize))
                .and_then(|offset| offset.checked_mul(3))
                .ok_or_else(|| "crop is too large".to_owned())?;
            let end = offset
                .checked_add(row_bytes)
                .ok_or_else(|| "crop is too large".to_owned())?;
            pixels.extend_from_slice(
                image
                    .data()
                    .get(offset..end)
                    .ok_or_else(|| "crop exceeds the page raster".to_owned())?,
            );
        }
        // ImageBuffer takes ownership of the Vec; PNG encoding needs no extra Arc allocation/copy.
        image::RgbImage::from_raw(width, height, pixels)
            .ok_or_else(|| "crop buffer has an invalid length".to_owned())
    }
}

/// Page shell and any recoverable native-text extraction failure.
#[derive(Debug)]
pub struct PreScannedPage {
    pub extracted: ExtractedPage,
    pub extraction_error: Option<PdfiumRuntimeError>,
}

/// Failures at the serialized PDFium runtime boundary.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Native task failures remain part of the shared runtime error vocabulary.
pub enum PdfiumRuntimeError {
    /// An IPC connection, worker lifecycle, or protocol invariant failed.
    #[error("PDFium worker transport failed: {0}")]
    Transport(String),
    /// A remote page operation preserved its original diagnostic message.
    #[error("{0}")]
    RemotePage(String),
    /// The operating-system path cannot be represented by the current PDFium wrapper.
    #[error("PDF path is not valid UTF-8")]
    NonUtf8Path,
    /// PDFium rejected the input document.
    #[error("failed to open PDF document: {0}")]
    OpenDocument(::pdfium::PdfiumError),
    /// The process could not load or initialize the PDFium shared library.
    #[error("failed to initialize PDFium: {0}")]
    Initialize(::pdfium::PdfiumError),
    /// A public one-based page request lies outside the document.
    #[error("page {page_number} is outside document range 1..={page_count}")]
    InvalidPage { page_number: u32, page_count: u32 },
    /// One page-level PDFium operation failed.
    #[error("PDFium {stage} failed on page {page_number}: {source}")]
    PageOperation {
        page_number: u32,
        stage: &'static str,
        #[source]
        source: ::pdfium::PdfiumError,
    },
    /// Native extraction rejected one character or geometry fact.
    #[error("native extraction failed on page {page_number}: {source}")]
    Extraction {
        page_number: u32,
        #[source]
        source: crate::ExtractError,
    },
    /// Canonical page geometry could not be constructed.
    #[error("page geometry failed on page {page_number}: {source}")]
    Geometry {
        page_number: u32,
        #[source]
        source: docparse_layout::GeometryError,
    },
    /// Rendered pixels did not satisfy the shared image contract.
    #[error("rendered image failed validation on page {page_number}: {source}")]
    PageImage {
        page_number: u32,
        #[source]
        source: docparse_layout::PageImageError,
    },
    /// The worker channel closed before delivering the requested response.
    #[error("PDFium worker stopped unexpectedly")]
    WorkerStopped,
    /// The dedicated blocking worker could not be created.
    #[error("failed to spawn PDFium worker: {0}")]
    ThreadSpawn(String),
    /// The worker panicked while owning PDFium resources.
    #[error("PDFium worker panicked")]
    WorkerPanicked,
    /// One operation exceeded its deadline while the worker was still running.
    ///
    /// The page keeps its native fallback; only the worker is unusable afterwards.
    #[error("PDFium {operation} exceeded {seconds} s")]
    OperationTimeout {
        operation: &'static str,
        seconds: u64,
    },
    /// An earlier deadline miss left the worker unusable, so this operation was not submitted.
    #[error("PDFium worker stopped responding during {operation}")]
    WorkerUnresponsive { operation: &'static str },
}

impl PdfiumRuntimeError {
    /// Reports a deadline miss, which leaves the page recoverable but the worker unusable.
    pub fn is_operation_timeout(&self) -> bool {
        matches!(
            self,
            Self::OperationTimeout { .. } | Self::WorkerUnresponsive { .. }
        )
    }

    /// Transport failures cannot be downgraded to page-level native fallback.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Transport(_)
                | Self::WorkerStopped
                | Self::WorkerPanicked
                | Self::ThreadSpawn(_)
        )
    }
}

impl From<oneshot::error::RecvError> for PdfiumRuntimeError {
    /// Preserves reply-channel diagnostics for both startup and command failures.
    fn from(error: oneshot::error::RecvError) -> Self {
        tracing::warn!("PDFium reply channel failed: {error}");
        Self::WorkerStopped
    }
}

/// Commands whose payloads contain only owned values and never PDFium handles.
pub(crate) enum PdfiumCommand {
    PreScan {
        page_number: u32,
        resolver: Option<Arc<dyn crate::GlyphResolver>>,
        response: oneshot::Sender<Result<PreScannedPage, PdfiumRuntimeError>>,
    },
    Render {
        page_lease: Option<docparse_common::PageLease>,
        page_number: u32,
        config: RenderConfig,
        response: oneshot::Sender<Result<RenderedPage, PdfiumRuntimeError>>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

/// Async facade over one dedicated thread that owns the complete PDFium lifetime.
#[derive(TypedBuilder)]
pub(crate) struct PdfiumExecutor {
    sender: mpsc::Sender<PdfiumCommand>,
    page_count: u32,
    #[builder(default, setter(strip_option))]
    worker: Option<PdfiumWorker>,
    /// Set once an operation misses its deadline; later operations fail without waiting again.
    unresponsive: AtomicBool,
}

impl PdfiumExecutor {
    /// Opens one document on a dedicated worker before returning its async facade.
    pub(crate) async fn open(
        input: PdfInput,
        _limits: &RuntimeConfig,
    ) -> Result<Self, PdfiumRuntimeError> {
        // Each document serializes commands; the render delivery queue owns page capacity.
        let (sender, receiver) = mpsc::channel(1);
        let (ready_sender, ready_receiver) = oneshot::channel();
        let worker = PdfiumWorker::spawn(input, receiver, ready_sender)?;
        let unresponsive = AtomicBool::new(false);
        let page_count = Self::await_reply(
            &unresponsive,
            async { ready_receiver.await.map_err(PdfiumRuntimeError::from) },
            "open",
            OPERATION_TIMEOUT,
        )
        .await??;
        Ok(Self::builder()
            .sender(sender)
            .page_count(page_count)
            .worker(worker)
            .unresponsive(unresponsive)
            .build())
    }

    /// Returns the fixed positive page count reported when the document opened.
    pub(crate) const fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Serially extracts one public one-based page into fully owned facts.
    pub(crate) async fn pre_scan_page(
        &self,
        page_number: u32,
        resolver: Option<Arc<dyn crate::GlyphResolver>>,
    ) -> Result<PreScannedPage, PdfiumRuntimeError> {
        self.validate_page(page_number)?;
        let (response, receiver) = oneshot::channel();
        self.request(
            PdfiumCommand::PreScan {
                page_number,
                // Only the owned resolver crosses the worker boundary, never a borrowed font.
                resolver,
                response,
            },
            receiver,
            "pre-scan",
            OPERATION_TIMEOUT,
        )
        .await?
    }

    /// Serially rasterizes one page and returns only owned RGB pixels and transforms.
    pub(crate) async fn render_page(
        &self,
        page_number: u32,
        config: &RenderConfig,
    ) -> Result<RenderedPage, PdfiumRuntimeError> {
        self.validate_page(page_number)?;
        let (response, receiver) = oneshot::channel();
        self.request(
            PdfiumCommand::Render {
                page_lease: docparse_common::PageLease::current(),
                page_number,
                config: config.clone(),
                response,
            },
            receiver,
            "render",
            OPERATION_TIMEOUT,
        )
        .await?
    }

    /// Requests orderly document shutdown and waits for worker cleanup without blocking the runtime.
    pub(crate) async fn close(mut self) -> Result<(), PdfiumRuntimeError> {
        // A worker that already missed a deadline is not asked again: waiting would add another
        // deadline to a parse that cannot use it any more.
        if self.unresponsive.load(Ordering::Acquire) {
            tracing::warn!(
                "PDFium worker for a {}-page document stopped responding; closing without waiting",
                self.page_count
            );
            return Ok(());
        }
        let (response, receiver) = oneshot::channel();
        let result = self
            .request(
                PdfiumCommand::Shutdown { response },
                receiver,
                "close",
                CLOSE_DEADLINE,
            )
            .await;
        // A shutdown deadline also covers queue admission; never add a join wait after it expires.
        if !self.unresponsive.load(Ordering::Acquire)
            && let Some(worker) = self.worker.take()
        {
            worker.join().await?;
        }
        result
    }

    /// Bounds queue admission and the reply together, rejecting unusable workers before sending.
    async fn request<M>(
        &self,
        command: PdfiumCommand,
        receiver: oneshot::Receiver<M>,
        operation: &'static str,
        deadline: Duration,
    ) -> Result<M, PdfiumRuntimeError> {
        Self::await_reply(
            &self.unresponsive,
            async {
                self.sender.send(command).await.map_err(|error| {
                    tracing::warn!("PDFium command send failed: {error}");
                    PdfiumRuntimeError::WorkerStopped
                })?;
                receiver.await.map_err(PdfiumRuntimeError::from)
            },
            operation,
            deadline,
        )
        .await
    }

    /// Runs one worker exchange under a deadline, including any queue admission.
    ///
    /// A missed deadline leaves the worker running but unusable, so the executor is marked and
    /// later operations fail without waiting again; the page itself keeps its native fallback.
    async fn await_reply<M>(
        unresponsive: &AtomicBool,
        reply: impl Future<Output = Result<M, PdfiumRuntimeError>>,
        operation: &'static str,
        deadline: Duration,
    ) -> Result<M, PdfiumRuntimeError> {
        if unresponsive.load(Ordering::Acquire) {
            return Err(PdfiumRuntimeError::WorkerUnresponsive { operation });
        }
        crate::wasm_compat::timeout(deadline, reply)
            .await
            .map_err(|_elapsed| {
                unresponsive.store(true, Ordering::Release);
                tracing::warn!(
                    "PDFium {} exceeded {} s; the worker is left detached and later pages use native fallback",
                    operation,
                    deadline.as_secs()
                );
                PdfiumRuntimeError::OperationTimeout {
                    operation,
                    seconds: deadline.as_secs(),
                }
            })?
    }

    /// Rejects invalid page numbers before they enter the actor queue.
    fn validate_page(
        &self,
        page_number: u32,
    ) -> Result<(), PdfiumRuntimeError> {
        if (1..=self.page_count).contains(&page_number) {
            Ok(())
        } else {
            Err(PdfiumRuntimeError::InvalidPage {
                page_number,
                page_count: self.page_count,
            })
        }
    }
}

/// Owns the library, source bytes, and document while serving serialized commands.
pub(crate) async fn worker_main(
    input: PdfInput,
    mut receiver: mpsc::Receiver<PdfiumCommand>,
    ready: oneshot::Sender<Result<u32, PdfiumRuntimeError>>,
) {
    // The parser boundary must return initialization errors instead of inheriting legacy panic semantics.
    let library = match Library::try_init() {
        Ok(library) => library,
        Err(error) => {
            let _ = ready.send(Err(PdfiumRuntimeError::Initialize(error)));
            return;
        }
    };
    let document = match input.open(&library) {
        Ok(document) => document,
        Err(error) => {
            tracing::error!("failed to open PDF: {}", error);
            let _ = ready.send(Err(error));
            return;
        }
    };
    let page_count = match u32::try_from(document.page_count()) {
        Ok(page_count) if page_count > 0 => page_count,
        _ => {
            let _ = ready.send(Err(PdfiumRuntimeError::OpenDocument(
                ::pdfium::PdfiumError::InvalidFormat,
            )));
            return;
        }
    };
    if ready.send(Ok(page_count)).is_err() {
        return;
    }

    while let Some(command) = receiver.recv().await {
        match command {
            PdfiumCommand::PreScan {
                page_number,
                resolver,
                response,
            } => {
                let _ = response.send(pre_scan_document_page(
                    &document,
                    page_number,
                    resolver.as_deref(),
                ));
            }
            PdfiumCommand::Render {
                page_lease,
                page_number,
                config,
                response,
            } => {
                // The real PDFium worker, including an uncollected reply, retains the delivery after caller cancellation.
                let render =
                    || render_document_page(&document, page_number, &config);
                let result = match &page_lease {
                    Some(lease) => lease.scope_sync(render),
                    None => render(),
                };
                let _ = response.send(result);
            }
            PdfiumCommand::Shutdown { response } => {
                let _ = response.send(());
                break;
            }
        }
    }
}

/// Extracts one page while every borrowed PDFium handle stays on the worker stack.
pub(crate) fn pre_scan_document_page(
    document: &Document<'_>,
    page_number: u32,
    resolver: Option<&dyn crate::GlyphResolver>,
) -> Result<PreScannedPage, PdfiumRuntimeError> {
    let page_index = i32::try_from(page_number.saturating_sub(1)).map_err(
        |_conversion_error| PdfiumRuntimeError::InvalidPage {
            page_number,
            page_count: u32::try_from(document.page_count())
                .unwrap_or_default(),
        },
    )?;
    let page = document.page(page_index).map_err(|source| {
        PdfiumRuntimeError::PageOperation {
            page_number,
            stage: "open page",
            source,
        }
    })?;
    let view_box =
        page.view_box().ok_or(PdfiumRuntimeError::PageOperation {
            page_number,
            stage: "read page box",
            source: ::pdfium::PdfiumError::OperationFailed,
        })?;
    let (width, height) = page.viewport_size(&view_box);
    let content_bounds = page
        .content_bounds()
        .map(|bounds| page.bounds_to_viewport(&view_box, &bounds))
        .and_then(|bounds| {
            Bbox::try_from([
                f64::from(bounds.left),
                f64::from(bounds.top),
                f64::from(bounds.right),
                f64::from(bounds.bottom),
            ])
            .ok()
        });
    let mut table_evidence = crate::TableEvidence::default();
    table_evidence.read_geometry(&page, &view_box);
    // Preserve page geometry even when only the native-text layer is unavailable.
    let (text_items, extraction_error) = match page.text() {
        Ok(text_page) => match extract_page_text_items(
            &page,
            &text_page,
            &view_box,
            page_number,
            &mut table_evidence,
            resolver,
        ) {
            Ok(text_items) => (text_items, None),
            Err(source) => (
                Vec::new(),
                Some(PdfiumRuntimeError::Extraction {
                    page_number,
                    source,
                }),
            ),
        },
        Err(source) => (
            Vec::new(),
            Some(PdfiumRuntimeError::PageOperation {
                page_number,
                stage: "load text page",
                source,
            }),
        ),
    };
    let extracted = ExtractedPage::builder()
        .page_number(page_number)
        .width(f64::from(width))
        .height(f64::from(height))
        .rotation(normalized_rotation(page.rotation()))
        .watermark_annotations(
            page.annotations(&view_box)
                .into_iter()
                .filter(|annotation| annotation.subtype == "watermark")
                .filter_map(|annotation| annotation.rect)
                .filter_map(|rect| {
                    Bbox::try_from([
                        f64::from(rect.left),
                        f64::from(rect.top),
                        f64::from(rect.right),
                        f64::from(rect.bottom),
                    ])
                    .ok()
                })
                .collect(),
        )
        .content_bounds(content_bounds)
        .text_items(text_items)
        .table_evidence(table_evidence)
        .build();
    // Keep document-wide scan facts lightweight; image payloads belong to render admission.
    Ok(PreScannedPage {
        extracted,
        extraction_error,
    })
}

/// Renders one page and derives transforms before dropping all PDFium handles.
pub(crate) fn render_document_page(
    document: &Document<'_>,
    page_number: u32,
    config: &RenderConfig,
) -> Result<RenderedPage, PdfiumRuntimeError> {
    let page_index = i32::try_from(page_number.saturating_sub(1)).map_err(
        |_conversion_error| PdfiumRuntimeError::InvalidPage {
            page_number,
            page_count: u32::try_from(document.page_count())
                .unwrap_or_default(),
        },
    )?;
    let page = document.page(page_index).map_err(|source| {
        PdfiumRuntimeError::PageOperation {
            page_number,
            stage: "open page",
            source,
        }
    })?;
    let view_box =
        page.view_box().ok_or(PdfiumRuntimeError::PageOperation {
            page_number,
            stage: "read page box",
            source: ::pdfium::PdfiumError::OperationFailed,
        })?;
    let (viewport_width, viewport_height) = page.viewport_size(&view_box);
    let long_edge = f64::from(viewport_width.max(viewport_height));
    let limited_dpi = if long_edge > 0.0 {
        (f64::from(config.max_long_edge_pixels) * 72.0 / long_edge)
            .min(f64::from(config.dpi))
    } else {
        f64::from(config.dpi)
    };
    let bitmap = page.render(limited_dpi as f32).map_err(|source| {
        PdfiumRuntimeError::PageOperation {
            page_number,
            stage: "render page",
            source,
        }
    })?;
    let render_width =
        u32::try_from(bitmap.width()).map_err(|_conversion_error| {
            PdfiumRuntimeError::PageOperation {
                page_number,
                stage: "read rendered width",
                source: ::pdfium::PdfiumError::OperationFailed,
            }
        })?;
    let render_height =
        u32::try_from(bitmap.height()).map_err(|_conversion_error| {
            PdfiumRuntimeError::PageOperation {
                page_number,
                stage: "read rendered height",
                source: ::pdfium::PdfiumError::OperationFailed,
            }
        })?;
    let rgb = Arc::<[u8]>::from(bitmap.to_rgb().map_err(|source| {
        PdfiumRuntimeError::PageOperation {
            page_number,
            stage: "read rendered pixels",
            source,
        }
    })?);
    let image = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(render_width)
                .height(render_height)
                .pixel_format(PixelFormat::Rgb8)
                .data(rgb)
                .build(),
        )
        .map_err(|source| PdfiumRuntimeError::PageImage {
            page_number,
            source,
        })?,
    );
    let (origin_x, origin_y) = page.page_to_viewport(&view_box, 0.0, 0.0);
    let (unit_x_x, unit_x_y) = page.page_to_viewport(&view_box, 1.0, 0.0);
    let (unit_y_x, unit_y_y) = page.page_to_viewport(&view_box, 0.0, 1.0);
    let transform = PageTransform::try_from(
        PageTransformInput::builder()
            .page_to_viewport(
                AffineTransform::builder()
                    .a(f64::from(unit_x_x - origin_x))
                    .b(f64::from(unit_y_x - origin_x))
                    .c(f64::from(unit_x_y - origin_y))
                    .d(f64::from(unit_y_y - origin_y))
                    .e(f64::from(origin_x))
                    .f(f64::from(origin_y))
                    .build(),
            )
            .viewport_width(f64::from(viewport_width))
            .viewport_height(f64::from(viewport_height))
            .render_width(render_width)
            .render_height(render_height)
            .model_width(800)
            .model_height(800)
            .rotation(page_rotation(page.rotation()))
            .build(),
    )
    .map_err(|source| PdfiumRuntimeError::Geometry {
        page_number,
        source,
    })?;
    let mut rendered = RenderedPage::builder()
        .page_number(page_number)
        .image(image)
        .transform(transform)
        .build();
    // Rendering already holds a completion-counted page lease, bounding retained image files.
    rendered.embedded_images = page
        .embedded_images(&view_box)
        .into_iter()
        .filter_map(|image| crate::figure::EmbeddedImage::try_from(image).ok())
        .collect();
    Ok(rendered)
}

/// Converts PDFium quarter-turn values into public clockwise degrees.
fn normalized_rotation(rotation: i32) -> i32 {
    rotation.rem_euclid(4) * 90
}

/// Converts PDFium quarter-turn values into the canonical rotation enum.
fn page_rotation(rotation: i32) -> PageRotation {
    match rotation.rem_euclid(4) {
        1 => PageRotation::Degrees90,
        2 => PageRotation::Degrees180,
        3 => PageRotation::Degrees270,
        _ => PageRotation::Degrees0,
    }
}

#[cfg(test)]
mod tests {
    /// An unusable worker must reject every operation without filling its abandoned queue.
    #[tokio::test]
    async fn unresponsive_worker_rejects_commands_before_enqueueing() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let executor = PdfiumExecutor::builder()
            .sender(sender)
            .page_count(1)
            .unresponsive(std::sync::atomic::AtomicBool::new(true))
            .build();
        for _ in 0..2 {
            assert!(
                executor
                    .render_page(1, &RenderConfig::default())
                    .await
                    .expect_err("unresponsive")
                    .is_operation_timeout()
            );
            assert!(
                matches!(
                    receiver.try_recv(),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ),
                "failed worker must not receive another command"
            );
            assert!(
                executor
                    .pre_scan_page(1, None)
                    .await
                    .expect_err("unresponsive")
                    .is_operation_timeout()
            );
            assert!(matches!(
                receiver.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        }
        executor.close().await.expect("close unusable worker");
    }

    /// A full command queue must consume the same deadline as the worker reply.
    #[tokio::test]
    async fn operation_deadline_includes_queue_admission() {
        let (sender, mut queued) = tokio::sync::mpsc::channel(1);
        let (response, _reply) = tokio::sync::oneshot::channel();
        sender
            .try_send(super::PdfiumCommand::Shutdown { response })
            .expect("fill queue");
        let executor = PdfiumExecutor::builder()
            .sender(sender)
            .page_count(1)
            .unresponsive(std::sync::atomic::AtomicBool::new(false))
            .build();
        let (response, reply) = tokio::sync::oneshot::channel();
        let error = executor
            .request(
                super::PdfiumCommand::Shutdown { response },
                reply,
                "close",
                std::time::Duration::from_millis(10),
            )
            .await
            .expect_err("admission deadline");
        assert!(error.is_operation_timeout());
        queued.try_recv().expect("original command");
        assert!(
            matches!(
                queued.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "timed-out admission must not enqueue its command later"
        );
        executor.close().await.expect("close unusable worker");
    }

    /// A missed deadline marks the worker and skips later waits instead of repeating them.
    #[tokio::test]
    async fn missed_deadline_marks_the_worker_unresponsive() {
        use crate::PdfiumRuntimeError;
        use std::{sync::atomic::AtomicBool, time::Duration};
        use tokio::sync::oneshot;

        let unresponsive = AtomicBool::new(false);
        let (_sender, receiver) = oneshot::channel::<u32>();
        let error = PdfiumExecutor::await_reply(
            &unresponsive,
            async { receiver.await.map_err(PdfiumRuntimeError::from) },
            "render",
            Duration::from_millis(10),
        )
        .await
        .expect_err("deadline");
        assert!(matches!(
            error,
            PdfiumRuntimeError::OperationTimeout {
                operation: "render",
                ..
            }
        ));
        assert!(
            !error.is_fatal(),
            "a deadline must keep the page recoverable"
        );

        let (_sender, receiver) = oneshot::channel::<u32>();
        let error = PdfiumExecutor::await_reply(
            &unresponsive,
            async { receiver.await.map_err(PdfiumRuntimeError::from) },
            "render",
            Duration::from_millis(10),
        )
        .await
        .expect_err("unresponsive");
        assert!(matches!(
            error,
            PdfiumRuntimeError::WorkerUnresponsive {
                operation: "render"
            }
        ));
        assert!(error.is_operation_timeout());
    }

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use docparse_config::{RenderConfig, RuntimeConfig};

    use super::{PdfInput, PdfiumExecutor};

    include!("../../tests/common/pdf.rs");

    /// Pre-scan must not retain image payloads; bounded rendering must still deliver them.
    #[tokio::test]
    async fn embedded_images_are_loaded_only_during_render() {
        let content = "q 40 0 0 40 10 10 cm /Im1 Do Q";
        let bytes = document(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".into(),
            // Use valid image samples now that extraction evaluates transparency rather than only sniffing JPEG markers.
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length 7 >>\nstream\nFF0000>\nendstream".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ]);
        let executor = PdfiumExecutor::open(
            PdfInput::Bytes(Arc::from(bytes)),
            &RuntimeConfig::default(),
        )
        .await
        .expect("open");
        let scanned = executor.pre_scan_page(1, None).await.expect("scan");
        assert!(
            scanned.extracted.embedded_images.is_empty(),
            "pre-scan must not accumulate images across the document"
        );
        let rendered = executor
            .render_page(1, &RenderConfig::default())
            .await
            .expect("render");
        assert_eq!(rendered.embedded_images.len(), 1);
        assert_eq!(
            rendered
                .embedded_images
                .first()
                .expect("embedded image")
                .bytes
                .as_deref(),
            Some(&[255, 0, 0, 255][..])
        );
        executor.close().await.expect("close");
    }

    /// Resolves the deterministic PDF extraction fixture from the core crate.
    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf/extraction_metadata.pdf")
    }

    /// Verifies path input stays open across pre-scan and render actor commands.
    #[tokio::test]
    async fn path_input_supports_full_document_lifecycle() {
        let executor = PdfiumExecutor::open(
            PdfInput::Path(fixture_path()),
            &RuntimeConfig::default(),
        )
        .await
        .expect("fixture document must open");

        assert_eq!(executor.page_count(), 1);
        let pre_scanned = executor
            .pre_scan_page(1, None)
            .await
            .expect("fixture page must extract");
        let rendered = executor
            .render_page(1, &RenderConfig::default())
            .await
            .expect("fixture page must render");

        assert!(pre_scanned.extraction_error.is_none());
        assert_eq!(pre_scanned.extracted.page_number, 1);
        assert!(!pre_scanned.extracted.text_items.is_empty());
        assert_eq!(rendered.page_number, 1);
        assert_eq!(
            rendered.image.data().len(),
            (rendered.image.width() * rendered.image.height() * 3) as usize
        );
        executor.close().await.expect("executor must close cleanly");
    }

    /// Verifies shared byte input remains alive until the actor drops its document.
    #[tokio::test]
    async fn byte_input_outlives_pdfium_document() {
        let bytes = Arc::<[u8]>::from(
            std::fs::read(fixture_path()).expect("fixture bytes must read"),
        );
        let executor = PdfiumExecutor::open(
            PdfInput::Bytes(bytes),
            &RuntimeConfig::default(),
        )
        .await
        .expect("fixture bytes must open");

        let first = executor
            .pre_scan_page(1, None)
            .await
            .expect("first extraction must succeed");
        let second = executor
            .pre_scan_page(1, None)
            .await
            .expect("second extraction must succeed");

        assert!(first.extraction_error.is_none());
        assert!(second.extraction_error.is_none());
        assert_eq!(first.extracted, second.extracted);
        executor.close().await.expect("executor must close cleanly");
    }

    /// Verifies invalid one-based page requests do not terminate the worker.
    #[tokio::test]
    async fn invalid_page_request_is_recoverable() {
        let executor = PdfiumExecutor::open(
            PdfInput::Path(fixture_path()),
            &RuntimeConfig::default(),
        )
        .await
        .expect("fixture document must open");

        let _zero_error = executor
            .pre_scan_page(0, None)
            .await
            .expect_err("page zero must fail");
        let _past_end_error = executor
            .pre_scan_page(2, None)
            .await
            .expect_err("past-end page must fail");
        let _page = executor
            .pre_scan_page(1, None)
            .await
            .expect("valid page must remain available");
        executor.close().await.expect("executor must close cleanly");
    }
}
