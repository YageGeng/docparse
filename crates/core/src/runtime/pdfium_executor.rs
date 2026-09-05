use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_layout::{
    AffineTransform, Bbox, PageImage, PageImageInput, PageRotation,
    PageTransform, PageTransformInput, PixelFormat,
};
use pdfium::{Document, Library};
use tokio::sync::{mpsc, oneshot};
use typed_builder::TypedBuilder;

use crate::ExtractedPage;
use crate::extract::text::extract_page_text_items;

/// Owned PDF input whose bytes remain alive for the complete worker lifetime.
#[derive(Debug, Clone)]
pub(crate) enum PdfInput {
    Path(PathBuf),
    Bytes(Arc<[u8]>),
}

/// Fully owned rendered page returned across the PDFium actor boundary.
#[derive(Debug, Clone, TypedBuilder)]
pub(crate) struct RenderedPage {
    pub(crate) page_number: u32,
    pub(crate) image: Arc<PageImage>,
    pub(crate) transform: PageTransform,
}

/// Page shell and any recoverable native-text extraction failure.
#[derive(Debug)]
pub(crate) struct PreScannedPage {
    pub(crate) extracted: ExtractedPage,
    pub(crate) extraction_error: Option<PdfiumRuntimeError>,
}

/// Failures at the serialized PDFium runtime boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PdfiumRuntimeError {
    /// The operating-system path cannot be represented by the current PDFium wrapper.
    #[error("PDF path is not valid UTF-8")]
    NonUtf8Path,
    /// PDFium rejected the input document.
    #[error("failed to open PDF document: {0}")]
    OpenDocument(pdfium::PdfiumError),
    /// The process could not load or initialize the PDFium shared library.
    #[error("failed to initialize PDFium: {0}")]
    Initialize(pdfium::PdfiumError),
    /// A public one-based page request lies outside the document.
    #[error("page {page_number} is outside document range 1..={page_count}")]
    InvalidPage { page_number: u32, page_count: u32 },
    /// One page-level PDFium operation failed.
    #[error("PDFium {stage} failed on page {page_number}: {source}")]
    PageOperation {
        page_number: u32,
        stage: &'static str,
        #[source]
        source: pdfium::PdfiumError,
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
}

/// Commands whose payloads contain only owned values and never PDFium handles.
enum PdfiumCommand {
    PreScan {
        page_number: u32,
        response: oneshot::Sender<Result<PreScannedPage, PdfiumRuntimeError>>,
    },
    Render {
        page_number: u32,
        config: RenderConfig,
        response: oneshot::Sender<Result<RenderedPage, PdfiumRuntimeError>>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

/// Async facade over one dedicated thread that owns the complete PDFium lifetime.
pub(crate) struct PdfiumExecutor {
    sender: mpsc::Sender<PdfiumCommand>,
    page_count: u32,
    worker: Option<JoinHandle<()>>,
}

impl PdfiumExecutor {
    /// Opens one document on a dedicated worker before returning its async facade.
    pub(crate) async fn open(
        input: PdfInput,
        limits: &RuntimeConfig,
    ) -> Result<Self, PdfiumRuntimeError> {
        let capacity = limits.render_queue_capacity.max(1);
        let (sender, receiver) = mpsc::channel(capacity);
        let (ready_sender, ready_receiver) = oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("docparse-pdfium".to_owned())
            .spawn(move || {
                worker_main(input, receiver, ready_sender);
            })
            .map_err(|error| {
                PdfiumRuntimeError::ThreadSpawn(error.to_string())
            })?;
        let page_count = ready_receiver
            .await
            .map_err(|_receive_error| PdfiumRuntimeError::WorkerStopped)??;
        Ok(Self {
            sender,
            page_count,
            worker: Some(worker),
        })
    }

    /// Returns the fixed positive page count reported when the document opened.
    pub(crate) const fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Serially extracts one public one-based page into fully owned facts.
    pub(crate) async fn pre_scan_page(
        &self,
        page_number: u32,
    ) -> Result<PreScannedPage, PdfiumRuntimeError> {
        self.validate_page(page_number)?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(PdfiumCommand::PreScan {
                page_number,
                response,
            })
            .await
            .map_err(|_send_error| PdfiumRuntimeError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_receive_error| PdfiumRuntimeError::WorkerStopped)?
    }

    /// Serially rasterizes one page and returns only owned RGB pixels and transforms.
    pub(crate) async fn render_page(
        &self,
        page_number: u32,
        config: &RenderConfig,
    ) -> Result<RenderedPage, PdfiumRuntimeError> {
        self.validate_page(page_number)?;
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(PdfiumCommand::Render {
                page_number,
                config: config.clone(),
                response,
            })
            .await
            .map_err(|_send_error| PdfiumRuntimeError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_receive_error| PdfiumRuntimeError::WorkerStopped)?
    }

    /// Requests orderly document shutdown and joins the blocking worker off-runtime.
    pub(crate) async fn close(mut self) -> Result<(), PdfiumRuntimeError> {
        let (response, receiver) = oneshot::channel();
        let sent = self
            .sender
            .send(PdfiumCommand::Shutdown { response })
            .await
            .is_ok();
        if sent {
            receiver
                .await
                .map_err(|_receive_error| PdfiumRuntimeError::WorkerStopped)?;
        }
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || worker.join())
                .await
                .map_err(|_join_error| PdfiumRuntimeError::WorkerPanicked)?
                .map_err(|_panic_payload| PdfiumRuntimeError::WorkerPanicked)?;
        }
        if sent {
            Ok(())
        } else {
            Err(PdfiumRuntimeError::WorkerStopped)
        }
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
fn worker_main(
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
    let document = match &input {
        PdfInput::Path(path) => {
            let Some(path) = path.to_str() else {
                let _ = ready.send(Err(PdfiumRuntimeError::NonUtf8Path));
                return;
            };
            library.load_document(path, None)
        }
        PdfInput::Bytes(bytes) => library.load_document_from_bytes(bytes, None),
    };
    let document = match document {
        Ok(document) => document,
        Err(error) => {
            let _ = ready.send(Err(PdfiumRuntimeError::OpenDocument(error)));
            return;
        }
    };
    let page_count = match u32::try_from(document.page_count()) {
        Ok(page_count) if page_count > 0 => page_count,
        _ => {
            let _ = ready.send(Err(PdfiumRuntimeError::OpenDocument(
                pdfium::PdfiumError::InvalidFormat,
            )));
            return;
        }
    };
    if ready.send(Ok(page_count)).is_err() {
        return;
    }

    while let Some(command) = receiver.blocking_recv() {
        match command {
            PdfiumCommand::PreScan {
                page_number,
                response,
            } => {
                let _ = response
                    .send(pre_scan_document_page(&document, page_number));
            }
            PdfiumCommand::Render {
                page_number,
                config,
                response,
            } => {
                let _ = response.send(render_document_page(
                    &document,
                    page_number,
                    &config,
                ));
            }
            PdfiumCommand::Shutdown { response } => {
                let _ = response.send(());
                break;
            }
        }
    }
}

/// Extracts one page while every borrowed PDFium handle stays on the worker stack.
fn pre_scan_document_page(
    document: &Document<'_>,
    page_number: u32,
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
            source: pdfium::PdfiumError::OperationFailed,
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
    // Preserve page geometry even when only the native-text layer is unavailable.
    let (text_items, extraction_error) = match page.text() {
        Ok(text_page) => match extract_page_text_items(
            &page,
            &text_page,
            &view_box,
            page_number,
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
    Ok(PreScannedPage {
        extracted: ExtractedPage::builder()
            .page_number(page_number)
            .width(f64::from(width))
            .height(f64::from(height))
            .rotation(normalized_rotation(page.rotation()))
            .content_bounds(content_bounds)
            .text_items(text_items)
            .build(),
        extraction_error,
    })
}

/// Renders one page and derives transforms before dropping all PDFium handles.
fn render_document_page(
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
            source: pdfium::PdfiumError::OperationFailed,
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
                source: pdfium::PdfiumError::OperationFailed,
            }
        })?;
    let render_height =
        u32::try_from(bitmap.height()).map_err(|_conversion_error| {
            PdfiumRuntimeError::PageOperation {
                page_number,
                stage: "read rendered height",
                source: pdfium::PdfiumError::OperationFailed,
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
    Ok(RenderedPage::builder()
        .page_number(page_number)
        .image(image)
        .transform(transform)
        .build())
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
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use docparse_config::{RenderConfig, RuntimeConfig};

    use super::{PdfInput, PdfiumExecutor};

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
            .pre_scan_page(1)
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
            .pre_scan_page(1)
            .await
            .expect("first extraction must succeed");
        let second = executor
            .pre_scan_page(1)
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
            .pre_scan_page(0)
            .await
            .expect_err("page zero must fail");
        let _past_end_error = executor
            .pre_scan_page(2)
            .await
            .expect_err("past-end page must fail");
        let _page = executor
            .pre_scan_page(1)
            .await
            .expect("valid page must remain available");
        executor.close().await.expect("executor must close cleanly");
    }
}
