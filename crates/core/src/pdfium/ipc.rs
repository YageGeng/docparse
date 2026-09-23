//! Versioned native IPC shared by the server and isolated PDFium executable.
use super::executor::{pre_scan_document_page, render_document_page};
use super::{PdfInput, PdfiumRuntimeError, PreScannedPage, RenderedPage};
use crate::GlyphResolver;
use docparse_config::RenderConfig;
use docparse_layout::{PageImage, PageImageInput, PageTransform, PixelFormat};
use ipc_channel::ipc::{
    IpcOneShotServer, IpcReceiver, IpcSender, IpcSharedMemory,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use typed_builder::TypedBuilder;

// Image metadata now distinguishes encoded files from deferred RGBA pixels.
pub const PROTOCOL_VERSION: u32 = 5;
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const WORKER_BINARY: &str = "docparse-pdfium-worker";

/// Small stdout bootstrap; the endpoint is never included in diagnostic logs.
#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub version: String,
    pub endpoint: String,
}
impl Hello {
    /// Rejects incompatible artifacts before transferring document input.
    pub fn validate(&self) -> Result<(), PdfiumRuntimeError> {
        if self.protocol != PROTOCOL_VERSION
            || self.version != PACKAGE_VERSION
            || self.endpoint.is_empty()
        {
            return Err(PdfiumRuntimeError::Transport(
                "incompatible PDFium worker handshake".into(),
            ));
        }
        Ok(())
    }
}

/// Transfers only channel ends owned by the child.
#[derive(Serialize, Deserialize)]
pub struct Bootstrap {
    pub commands: IpcReceiver<Request>,
    pub responses: IpcSender<Reply>,
}

/// Source bytes remain mapped until the child drops its borrowed document.
#[derive(Debug, Serialize, Deserialize)]
pub enum Source {
    Path(PathBuf),
    Bytes(IpcSharedMemory),
}
impl TryFrom<PdfInput> for Source {
    type Error = PdfiumRuntimeError;
    /// Resolves paths before crossing processes and shares memory input.
    fn try_from(input: PdfInput) -> Result<Self, Self::Error> {
        match input {
            PdfInput::Path(path) => Ok(Self::Path(std::path::absolute(path)?)),
            PdfInput::Bytes(bytes) => {
                Ok(Self::Bytes(IpcSharedMemory::from_bytes(&bytes)))
            }
        }
    }
}
impl Source {
    /// Borrows storage and the library for the complete document lifetime.
    fn open<'a>(
        &'a self,
        library: &'a ::pdfium::Library,
    ) -> Result<::pdfium::Document<'a>, PdfiumRuntimeError> {
        match self {
            Self::Path(path) => library.load_document(
                path.to_str().ok_or(PdfiumRuntimeError::NonUtf8Path)?,
                None,
            ),
            Self::Bytes(bytes) => library.load_document_from_bytes(bytes, None),
        }
        .map_err(PdfiumRuntimeError::OpenDocument)
    }
}

/// Document-affine operations; glyph replies belong to the active scan request.
#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    Open(Source),
    PreScan {
        page_number: u32,
        resolve_glyphs: bool,
    },
    Render {
        page_number: u32,
        config: RenderConfig,
    },
    GlyphReply(Option<String>),
    Close,
    Shutdown,
}

/// Correlation survives document reuse; new processes receive fresh channels.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub lease: u64,
    pub id: u64,
    pub command: Command,
}

/// Validated transforms and an immutable shared pixel segment.
#[derive(Debug, Serialize, Deserialize, TypedBuilder)]
pub struct Raster {
    pub page_number: u32,
    pub transform: PageTransform,
    pub pixels: IpcSharedMemory,
    /// Metadata stays in the render message while original files and deferred pixels use shared memory.
    #[builder(default, setter(skip))]
    pub(crate) images:
        Vec<(crate::figure::EmbeddedImage, Option<IpcSharedMemory>)>,
}
impl From<RenderedPage> for Raster {
    /// Publishes pixels without serializing their byte array into the IPC message.
    fn from(page: RenderedPage) -> Self {
        let mut raster = Self::builder()
            .page_number(page.page_number)
            .transform(page.transform)
            .pixels(IpcSharedMemory::from_bytes(page.image.data()))
            .build();
        // Pair each payload with its metadata instead of maintaining parallel scan arrays.
        raster.images = page
            .embedded_images
            .into_iter()
            .map(|mut image| {
                let bytes = image
                    .bytes
                    .take()
                    .map(|bytes| IpcSharedMemory::from_bytes(&bytes));
                (image, bytes)
            })
            .collect();
        raster
    }
}
impl TryFrom<Raster> for RenderedPage {
    type Error = PdfiumRuntimeError;
    /// Checks pixel length before copying into the existing owned image representation.
    fn try_from(value: Raster) -> Result<Self, Self::Error> {
        let copying = std::time::Instant::now();
        let (width, height) = value.transform.render_size();
        let expected = usize::try_from(width)
            .ok()
            .and_then(|w| {
                usize::try_from(height).ok().and_then(|h| w.checked_mul(h))
            })
            .and_then(|n| n.checked_mul(3));
        if value.page_number == 0 || expected != Some(value.pixels.len()) {
            return Err(PdfiumRuntimeError::Transport(
                "invalid IPC page or raster buffer length".into(),
            ));
        }
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(width)
                .height(height)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(&*value.pixels))
                .build(),
        )
        .map_err(|source| PdfiumRuntimeError::PageImage {
            page_number: value.page_number,
            source,
        })?;
        tracing::debug!(
            "copied PDFium page {} raster ({} bytes) in {:.3} ms",
            value.page_number,
            value.pixels.len(),
            copying.elapsed().as_secs_f64() * 1000.0
        );
        let mut rendered = Self::builder()
            .page_number(value.page_number)
            .transform(value.transform)
            .image(Arc::new(image))
            .build();
        // Restore payloads only for this admitted page; pre-scan carries no image buffers.
        rendered.embedded_images = value
            .images
            .into_iter()
            .map(|(mut image, bytes)| {
                image.bytes = bytes.map(|memory| memory.to_vec());
                image
            })
            .collect();
        Ok(rendered)
    }
}

/// Snapshot JSON preserves canonical Serde omission and tagged-enum rules.
#[derive(Debug, Serialize, Deserialize)]
pub enum Outcome {
    Ready,
    Opened(u32),
    Scanned {
        json: Vec<u8>,
        extraction_error: Option<String>,
    },
    Rendered(Raster),
    Closed,
    Glyph(Vec<(i32, f32, f32)>),
    Failure {
        message: String,
        fatal: bool,
    },
}
impl TryFrom<PreScannedPage> for Outcome {
    type Error = PdfiumRuntimeError;
    /// Encodes transient facts separately from postcard-based IPC framing.
    fn try_from(page: PreScannedPage) -> Result<Self, Self::Error> {
        Ok(Self::Scanned {
            json: serde_json::to_vec(&page.extracted)?,
            extraction_error: page.extraction_error.map(|e| e.to_string()),
        })
    }
}

impl TryFrom<Outcome> for PreScannedPage {
    type Error = PdfiumRuntimeError;
    /// Restores lightweight facts; original image files arrive only with render responses.
    fn try_from(outcome: Outcome) -> Result<Self, Self::Error> {
        match outcome {
            Outcome::Scanned {
                json,
                extraction_error,
            } => Ok(Self {
                extracted: serde_json::from_slice(&json)?,
                extraction_error: extraction_error
                    .map(PdfiumRuntimeError::RemotePage),
            }),
            _ => Err(PdfiumRuntimeError::Transport(
                "expected PDFium scan response".into(),
            )),
        }
    }
}

/// Responses and reverse requests identify the originating document and request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub lease: u64,
    pub id: u64,
    pub outcome: Outcome,
}

impl From<ipc_channel::IpcError> for PdfiumRuntimeError {
    /// Preserves transport failures as fatal rather than page-recoverable errors.
    fn from(error: ipc_channel::IpcError) -> Self {
        Self::Transport(error.to_string())
    }
}
impl From<std::io::Error> for PdfiumRuntimeError {
    /// Reports startup and channel failures through the shared runtime boundary.
    fn from(error: std::io::Error) -> Self {
        Self::Transport(error.to_string())
    }
}
impl From<serde_json::Error> for PdfiumRuntimeError {
    /// Rejects malformed handshakes and snapshots at the IPC boundary.
    fn from(error: serde_json::Error) -> Self {
        Self::Transport(error.to_string())
    }
}

/// Mutexes satisfy the resolver's Sync contract; PDFium runs on one thread.
struct Connection {
    commands: Mutex<IpcReceiver<Request>>,
    responses: Mutex<IpcSender<Reply>>,
}
impl Connection {
    /// Releases the receiver lock before executing any PDFium operation.
    fn receive(&self) -> Result<Request, PdfiumRuntimeError> {
        Ok(self
            .commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recv()?)
    }

    /// Sends a correlated reply without logging its content.
    fn reply(
        &self,
        lease: u64,
        id: u64,
        outcome: Outcome,
    ) -> Result<(), PdfiumRuntimeError> {
        Ok(self
            .responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(Reply { lease, id, outcome })?)
    }
    /// Releases document resources before acknowledging Close or Shutdown; returns whether to exit.
    fn document(
        &self,
        library: &::pdfium::Library,
        source: Source,
        lease: u64,
        open_id: u64,
    ) -> Result<bool, PdfiumRuntimeError> {
        let document = match source.open(library) {
            Ok(document) => document,
            Err(error) => {
                tracing::warn!(
                    "PDFium worker could not open lease {}: {}",
                    lease,
                    error
                );
                self.reply(
                    lease,
                    open_id,
                    Outcome::Failure {
                        message: error.to_string(),
                        fatal: false,
                    },
                )?;
                return Ok(false);
            }
        };
        let page_count = match u32::try_from(document.page_count())
            .ok()
            .filter(|n| *n > 0)
        {
            Some(count) => count,
            None => {
                // A rejected document does not imply the persistent PDFium process is unhealthy.
                drop(document);
                drop(source);
                let error = PdfiumRuntimeError::OpenDocument(
                    ::pdfium::PdfiumError::InvalidFormat,
                );
                tracing::warn!(
                    "PDFium worker rejected empty lease {}: {}",
                    lease,
                    error
                );
                self.reply(
                    lease,
                    open_id,
                    Outcome::Failure {
                        message: error.to_string(),
                        fatal: false,
                    },
                )?;
                return Ok(false);
            }
        };
        self.reply(lease, open_id, Outcome::Opened(page_count))?;
        let mut last_id = open_id;
        let close = loop {
            let request = self.receive()?;
            // Shutdown is process-wide and follows any in-flight operation on the same bridge.
            let expected_lease = if matches!(request.command, Command::Shutdown)
            {
                0
            } else {
                lease
            };
            if request.lease != expected_lease || request.id <= last_id {
                return Err(PdfiumRuntimeError::Transport(
                    "stale PDFium document request".into(),
                ));
            }
            last_id = request.id;
            let result = match request.command {
                Command::Close | Command::Shutdown => break request,
                Command::PreScan {
                    page_number,
                    resolve_glyphs,
                } => {
                    let proxy = GlyphProxy::builder()
                        .connection(self)
                        .lease(lease)
                        .id(request.id)
                        .build();
                    let page = if (1..=page_count).contains(&page_number) {
                        pre_scan_document_page(
                            &document,
                            page_number,
                            resolve_glyphs
                                .then_some(&proxy as &dyn GlyphResolver),
                        )
                    } else {
                        Err(PdfiumRuntimeError::InvalidPage {
                            page_number,
                            page_count,
                        })
                    };
                    if let Some(error) = proxy
                        .failure
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                    {
                        return Err(PdfiumRuntimeError::Transport(error));
                    }
                    page.and_then(Outcome::try_from)
                }
                Command::Render {
                    page_number,
                    config,
                } => {
                    if !(1..=page_count).contains(&page_number) {
                        Err(PdfiumRuntimeError::InvalidPage {
                            page_number,
                            page_count,
                        })
                    } else if config.dpi == 0
                        || config.max_long_edge_pixels == 0
                    {
                        return Err(PdfiumRuntimeError::Transport(
                            "invalid worker render configuration".into(),
                        ));
                    } else {
                        render_document_page(&document, page_number, &config)
                            .map(|page| Outcome::Rendered(page.into()))
                    }
                }
                _ => {
                    return Err(PdfiumRuntimeError::Transport(
                        "unexpected command inside PDFium document".into(),
                    ));
                }
            };
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::warn!(
                        "PDFium lease {} request {} failed: {}",
                        lease,
                        request.id,
                        error
                    );
                    if error.is_fatal() {
                        return Err(error);
                    }
                    Outcome::Failure {
                        message: error.to_string(),
                        fatal: false,
                    }
                }
            };
            self.reply(lease, request.id, outcome)?;
        };
        drop(document);
        drop(source);
        self.reply(close.lease, close.id, Outcome::Closed)?;
        Ok(matches!(close.command, Command::Shutdown))
    }
}

/// Calls the parent's resolver while PDFium handles stay inside this process.
#[derive(TypedBuilder)]
struct GlyphProxy<'a> {
    id: u64,
    lease: u64,
    connection: &'a Connection,
    #[builder(default)]
    failure: Mutex<Option<String>>,
}

impl GlyphResolver for GlyphProxy<'_> {
    /// A failed reverse request invalidates the complete scan rather than dropping recovered text.
    fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
        if self
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return None;
        }
        let result = (|| -> Result<Option<String>, PdfiumRuntimeError> {
            self.connection.reply(
                self.lease,
                self.id,
                Outcome::Glyph(segments.to_vec()),
            )?;
            let response = self.connection.receive()?;
            match response.command {
                Command::GlyphReply(value)
                    if response.lease == self.lease
                        && response.id == self.id =>
                {
                    Ok(value)
                }
                _ => Err(PdfiumRuntimeError::Transport(
                    "invalid glyph reply".into(),
                )),
            }
        })();
        match result {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    "PDFium glyph resolver transport failed: {}",
                    error
                );
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(error.to_string());
                None
            }
        }
    }
}

/// Runs PDFium only, without constructing application runtimes or inference engines.
pub fn run_worker() -> Result<(), PdfiumRuntimeError> {
    let (bootstrap, endpoint) = IpcOneShotServer::<Bootstrap>::new()?;
    let hello = Hello {
        protocol: PROTOCOL_VERSION,
        version: PACKAGE_VERSION.into(),
        endpoint,
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &hello)?;
    writeln!(stdout)?;
    stdout.flush()?;
    drop(stdout);
    let (_, channels) = bootstrap.accept()?;
    let connection = Connection {
        commands: Mutex::new(channels.commands),
        responses: Mutex::new(channels.responses),
    };
    let library = ::pdfium::Library::try_init()
        .map_err(PdfiumRuntimeError::Initialize)?;
    connection.reply(0, 0, Outcome::Ready)?;
    tracing::info!(
        "PDFium worker {} ready without inference models",
        std::process::id()
    );
    let mut last_lease = 0;
    loop {
        let request = connection.receive()?;
        match request.command {
            Command::Shutdown => {
                connection.reply(request.lease, request.id, Outcome::Closed)?;
                break;
            }
            Command::Open(source) if request.lease > last_lease => {
                last_lease = request.lease;
                if connection.document(
                    &library,
                    source,
                    request.lease,
                    request.id,
                )? {
                    break;
                }
            }
            _ => {
                return Err(PdfiumRuntimeError::Transport(
                    "expected a new PDFium document lease".into(),
                ));
            }
        }
    }
    tracing::info!("PDFium worker {} stopped", std::process::id());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::{
        AffineTransform, Bbox, PageRotation, PageTransformInput,
    };

    /// Render IPC preserves original files and RGBA pixels without relying on JSON field omission.
    #[test]
    fn image_files_round_trip_with_render_ipc() {
        let transform = PageTransform::try_from(
            PageTransformInput::builder()
                .page_to_viewport(AffineTransform::identity())
                .viewport_width(4.0)
                .viewport_height(4.0)
                .render_width(4)
                .render_height(4)
                .model_width(4)
                .model_height(4)
                .rotation(PageRotation::Degrees0)
                .build(),
        )
        .expect("transform");
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(4)
                .height(4)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(vec![255_u8; 48]))
                .build(),
        )
        .expect("raster");
        let mut rendered = RenderedPage::builder()
            .page_number(1)
            .image(Arc::new(image))
            .transform(transform)
            .build();
        rendered.embedded_images = [
            Some(crate::figure::EmbeddedImageFormat::Encoded(
                crate::FigureMediaType::Jpeg,
            )),
            Some(crate::figure::EmbeddedImageFormat::Rgba),
            None,
        ]
        .into_iter()
        .map(|format| {
            crate::figure::EmbeddedImage::builder()
                .bounds(Bbox::try_from([1.0, 2.0, 3.0, 4.0]).expect("bounds"))
                .pixel_width(2)
                .pixel_height(1)
                .format(format)
                .bytes(format.map(|format| match format {
                    crate::figure::EmbeddedImageFormat::Rgba => {
                        vec![255, 0, 0, 0, 0, 255, 0, 255]
                    }
                    crate::figure::EmbeddedImageFormat::Encoded(_) => {
                        vec![0xff, 0xd8, 0xff, 0xd9]
                    }
                }))
                .build()
        })
        .collect();
        let expected = rendered.embedded_images.clone();
        let (sender, receiver) = ipc_channel::ipc::channel().expect("channel");
        sender.send(Raster::from(rendered)).expect("send");
        let restored =
            RenderedPage::try_from(receiver.recv().expect("receive"))
                .expect("restore");
        assert_eq!(restored.embedded_images, expected);
    }
}
