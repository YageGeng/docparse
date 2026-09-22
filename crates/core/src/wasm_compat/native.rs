//! Native filesystem parsing and overlay output, selected by the compatibility entry point.
use super::PdfInput;
use crate::pdfium::PdfiumExecutor;
use crate::{DocParseError, DocParser, DocumentResult};
use crate::{OverlayRenderer, RenderError};
use docparse_config::ValidatedConfig;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{io::Write, sync::Mutex};

impl DocParser {
    /// Parses one filesystem PDF while retaining path context on failure.
    pub async fn parse_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<DocumentResult, DocParseError> {
        let path = path.as_ref().to_path_buf();
        self.runtime()
            .parse_document_with_options(
                PdfInput::Path(path.clone()),
                crate::ParseOptions::default(),
            )
            .await
            .map_err(DocParseError::from)
            .map_err(|source| DocParseError::ParsePath {
                path,
                source: Box::new(source),
            })
    }
    /// Parses a native path with per-call table policy and an optional external structure engine.
    pub async fn parse_path_with_options(
        &self,
        path: impl AsRef<Path>,
        options: crate::ParseOptions<'_>,
    ) -> Result<DocumentResult, DocParseError> {
        let path = path.as_ref().to_path_buf();
        self.runtime()
            .parse_document_with_options(PdfInput::Path(path.clone()), options)
            .await
            .map_err(DocParseError::from)
            .map_err(|source| DocParseError::ParsePath {
                path,
                source: Box::new(source),
            })
    }

    /// Runs path parsing without nesting a Tokio runtime.
    pub fn parse_path_blocking(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<DocumentResult, DocParseError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(DocParseError::BlockingInsideRuntime);
        }
        let path = path.as_ref().to_path_buf();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(DocParseError::BlockingRuntime)?;
        runtime.block_on(self.parse_path(path))
    }
}

/// Reopens one PDF and writes per-page PNG/SVG overlays without rerunning layout inference.
pub async fn write_pdf_overlays(
    config: &ValidatedConfig,
    input: &Path,
    document: &crate::DocumentResult,
    output_dir: &Path,
) -> Result<Vec<PathBuf>, RenderError> {
    write_pdf_overlays_for_pages(config, input, document, output_dir, None)
        .await
}

/// Writes a deterministic subset of page overlays while still validating source page count.
pub async fn write_pdf_overlays_for_pages(
    config: &ValidatedConfig,
    input: &Path,
    document: &crate::DocumentResult,
    output_dir: &Path,
    page_numbers: Option<&BTreeSet<u32>>,
) -> Result<Vec<PathBuf>, RenderError> {
    std::fs::create_dir_all(output_dir)?;
    let executor = PdfiumExecutor::open(
        PdfInput::Path(input.to_path_buf()),
        config.runtime(),
    )
    .await
    .map_err(|error| RenderError::Pdfium(error.to_string()))?;
    let operation = async {
        if usize::try_from(executor.page_count()).unwrap_or(usize::MAX)
            != document.pages.len()
        {
            return Err(RenderError::PageMismatch(format!(
                "source has {} pages but result has {}",
                executor.page_count(),
                document.pages.len()
            )));
        }
        let mut outputs = Vec::with_capacity(document.pages.len() * 2);
        for page in &document.pages {
            if page_numbers
                .is_some_and(|selected| !selected.contains(&page.page_number))
            {
                continue;
            }
            let rendered = executor
                .render_page(page.page_number, config.render())
                .await
                .map_err(|error| RenderError::Pdfium(error.to_string()))?;
            let png_name = format!("page-{:04}.png", page.page_number);
            let svg_name = format!("page-{:04}.svg", page.page_number);
            let artifacts = OverlayRenderer::render_page(
                rendered.image.as_ref(),
                page,
                &png_name,
            )?;
            let png_path = output_dir.join(png_name);
            let svg_path = output_dir.join(svg_name);
            std::fs::write(&png_path, artifacts.png)?;
            std::fs::write(&svg_path, artifacts.svg)?;
            outputs.push(png_path);
            outputs.push(svg_path);
        }
        Ok(outputs)
    }
    .await;
    let close_result = executor
        .close()
        .await
        .map_err(|error| RenderError::Pdfium(error.to_string()));
    let outputs = operation?;
    close_result?;
    Ok(outputs)
}

/// Lazily owns one parse's image directory until the caller commits its result.
/// Clones of its Arc retain files through cancelled, still-running CPU writes.
pub struct FigureAssets {
    pub(crate) config: docparse_config::FigureConfig,
    prefix: String,
    directory: Mutex<Option<tempfile::TempDir>>,
}

impl FigureAssets {
    /// Creates a lazy owner without touching disk, including for file-free documents.
    pub(crate) fn new(
        config: docparse_config::FigureConfig,
        prefix: String,
    ) -> Self {
        Self {
            config,
            prefix,
            directory: Mutex::new(None),
        }
    }

    /// Retains files after successful publication; otherwise the last owner removes them.
    /// Pass false when durable publication is uncertain so recovery can reconcile the pending marker.
    /// Call only after all parse and publication work using this owner has completed.
    pub fn keep(&self, committed: bool) {
        if let Some(directory) = self
            .directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
        {
            // A pending marker permits server recovery after a crash or an ambiguous database acknowledgement.
            if committed
                && let Err(error) =
                    std::fs::remove_file(directory.path().join(".pending"))
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    "failed to clear figure publication marker: {}",
                    error
                );
            }
            // Retain the path so an uncertain publication can later clear its marker idempotently.
            directory.disable_cleanup(true);
        }
    }

    /// Atomically writes an image and automatically removes incomplete temporary files on failure.
    pub(crate) fn write(
        &self,
        block_id: &str,
        media_type: crate::FigureMediaType,
        bytes: &[u8],
    ) -> Result<String, String> {
        let mut directory = self.directory.lock().map_err(|error| {
            format!("figure directory lock is poisoned: {error}")
        })?;
        if directory.is_none() {
            let root = self.config.directory.as_deref().ok_or_else(|| {
                "figures.directory is required for file delivery".to_owned()
            })?;
            if self.prefix.contains(['/', '\\']) {
                return Err(
                    "figure directory prefix must not contain path separators"
                        .to_owned(),
                );
            }
            std::fs::create_dir_all(root).map_err(|error| {
                format!("failed to create figure root: {error}")
            })?;
            let root = std::fs::canonicalize(root).map_err(|error| {
                format!("failed to resolve figure root: {error}")
            })?;
            let created = tempfile::Builder::new()
                .prefix(&self.prefix)
                .tempdir_in(root)
                .map_err(|error| {
                    format!("failed to create figure directory: {error}")
                })?;
            std::fs::write(created.path().join(".pending"), []).map_err(
                |error| format!("failed to mark figure publication: {error}"),
            )?;
            *directory = Some(created);
        }
        let directory =
            directory.as_ref().expect("initialized image directory");
        let stem: String = block_id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        let path = directory
            .path()
            .join(format!("{stem}.{}", media_type.extension()));
        let mut temporary =
            tempfile::NamedTempFile::new_in(directory.path())
                .map_err(|error| format!("failed to create figure: {error}"))?;
        temporary
            .write_all(bytes)
            .map_err(|error| format!("failed to write figure: {error}"))?;
        temporary
            .persist(&path)
            .map_err(|error| format!("failed to publish figure: {error}"))?;
        Ok(path.to_string_lossy().into_owned())
    }
}

impl crate::FigureMediaType {
    /// Returns the file extension used for directory delivery.
    pub(crate) const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Jp2 => "jp2",
            Self::Jpx => "jpx",
        }
    }
}
