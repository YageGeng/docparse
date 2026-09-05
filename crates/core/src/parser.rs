use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use docparse_config::ValidatedConfig;
use docparse_layout::{
    LayoutEngine, PageImage, PageTransform, PpDocLayoutV3Engine,
};
use typed_builder::TypedBuilder;

use crate::runtime::{
    ParseRuntime, ParseRuntimeError, PdfInput, RenderedPage,
    analyze_rendered_page,
};
use crate::{
    DocumentContextBuilder, DocumentResult, ExtractedPage, OcrEngine,
    PageProbe, PageResult,
};

/// Top-level parser failures with source chains preserved across runtime layers.
#[derive(Debug, thiserror::Error)]
pub enum DocParseError {
    /// A builder cannot construct a parser without validated configuration.
    #[error("DocParserBuilder requires validated configuration")]
    MissingConfiguration,
    /// The default or injected layout engine failed during initialization.
    #[error(transparent)]
    Layout(#[from] docparse_layout::LayoutError),
    /// The document pipeline encountered a non-degradable runtime error.
    #[error("document runtime failed: {source}")]
    Runtime {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A path parse failed and retains the caller-visible input location.
    #[error("failed to parse PDF {}: {source}", path.display())]
    ParsePath {
        path: PathBuf,
        #[source]
        source: Box<DocParseError>,
    },
    /// A standalone page could not produce a valid context or page result.
    #[error("standalone page parse failed: {source}")]
    ParsePage {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Blocking wrappers cannot safely create a nested Tokio runtime.
    #[error("blocking parser API cannot be called from inside a Tokio runtime")]
    BlockingInsideRuntime,
    /// Tokio could not build the current-thread runtime for a blocking wrapper.
    #[error("failed to create blocking parser runtime: {0}")]
    BlockingRuntime(std::io::Error),
}

impl From<ParseRuntimeError> for DocParseError {
    /// Boxes the private runtime error while retaining its complete source chain.
    fn from(source: ParseRuntimeError) -> Self {
        Self::Runtime {
            source: Box::new(source),
        }
    }
}

/// Fully owned standalone page input shared with layout and optional OCR engines.
#[derive(Debug, Clone, TypedBuilder)]
pub struct PageInput {
    pub extracted: ExtractedPage,
    pub image: Arc<PageImage>,
    pub transform: PageTransform,
}

/// Public reusable parser with immutable shared configuration and engines.
#[derive(Clone)]
pub struct DocParser {
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    ocr_engine: Option<Arc<dyn OcrEngine>>,
}

impl fmt::Debug for DocParser {
    /// Formats stable engine identity without requiring trait-object Debug implementations.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocParser")
            .field("layout_engine", &self.layout_engine.name())
            .field(
                "ocr_engine",
                &self.ocr_engine.as_ref().map(|engine| engine.name()),
            )
            .finish_non_exhaustive()
    }
}

/// Consuming dependency-injection builder for one reusable parser instance.
#[derive(Default)]
pub struct DocParserBuilder {
    config: Option<Arc<ValidatedConfig>>,
    layout_engine: Option<Arc<dyn LayoutEngine>>,
    ocr_engine: Option<Arc<dyn OcrEngine>>,
}

impl DocParserBuilder {
    /// Sets the mandatory immutable validated configuration.
    pub fn config(mut self, config: Arc<ValidatedConfig>) -> Self {
        self.config = Some(config);
        self
    }

    /// Injects a layout engine and bypasses default model artifact loading.
    pub fn layout_engine(mut self, engine: Arc<dyn LayoutEngine>) -> Self {
        self.layout_engine = Some(engine);
        self
    }

    /// Injects the optional single OCR engine used for missing regions.
    pub fn ocr_engine(mut self, engine: Arc<dyn OcrEngine>) -> Self {
        self.ocr_engine = Some(engine);
        self
    }

    /// Builds a parser, loading the default PP-DocLayoutV3 engine only when absent.
    pub async fn build(self) -> Result<DocParser, DocParseError> {
        let config = self.config.ok_or(DocParseError::MissingConfiguration)?;
        let layout_engine: Arc<dyn LayoutEngine> = match self.layout_engine {
            Some(engine) => engine,
            None => Arc::new(
                PpDocLayoutV3Engine::from_config(Arc::clone(&config)).await?,
            ),
        };
        Ok(DocParser {
            config,
            layout_engine,
            ocr_engine: self.ocr_engine,
        })
    }
}

impl DocParser {
    /// Creates an empty dependency-injection builder.
    pub fn builder() -> DocParserBuilder {
        DocParserBuilder::default()
    }

    /// Builds a parser with the default PP-DocLayoutV3 engine.
    pub async fn from_config(
        config: ValidatedConfig,
    ) -> Result<Self, DocParseError> {
        Self::builder().config(Arc::new(config)).build().await
    }

    /// Parses one filesystem PDF while retaining path context on failure.
    pub async fn parse_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<DocumentResult, DocParseError> {
        let path = path.as_ref().to_path_buf();
        self.runtime()
            .parse_document(PdfInput::Path(path.clone()))
            .await
            .map_err(DocParseError::from)
            .map_err(|source| DocParseError::ParsePath {
                path,
                source: Box::new(source),
            })
    }

    /// Parses one shared in-memory PDF without copying its bytes.
    pub async fn parse_bytes(
        &self,
        bytes: Arc<[u8]>,
    ) -> Result<DocumentResult, DocParseError> {
        self.runtime()
            .parse_document(PdfInput::Bytes(bytes))
            .await
            .map_err(DocParseError::from)
    }

    /// Parses one already extracted and rendered page with a one-page context.
    pub async fn parse_page(
        &self,
        input: PageInput,
    ) -> Result<PageResult, DocParseError> {
        let source_page_number = input.extracted.page_number;
        let mut context_builder = DocumentContextBuilder::builder()
            .page_count(1)
            .metadata(BTreeMap::from([(
                "standalone_page_number".to_owned(),
                source_page_number.to_string(),
            )]))
            .model_revision(Some(
                self.layout_engine.model_revision().to_owned(),
            ))
            .build();
        // Context statistics are one-page local while the returned page keeps its source identity.
        let mut probe = PageProbe::from(&input.extracted);
        probe.page_number = 1;
        context_builder.push_page(probe).map_err(|source| {
            DocParseError::ParsePage {
                source: Box::new(source),
            }
        })?;
        let context = context_builder.build().map_err(|source| {
            DocParseError::ParsePage {
                source: Box::new(source),
            }
        })?;
        let rendered = RenderedPage::builder()
            .page_number(input.extracted.page_number)
            .image(input.image)
            .transform(input.transform)
            .build();
        analyze_rendered_page(
            Arc::clone(&self.config),
            Arc::clone(&self.layout_engine),
            self.ocr_engine.as_ref().map(Arc::clone),
            context,
            input.extracted,
            rendered,
        )
        .await
        .map_err(|source| DocParseError::ParsePage {
            source: Box::new(source),
        })
    }

    /// Runs the async path parser from an ordinary synchronous thread.
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

    /// Creates one short-lived runtime facade that clones only shared ownership handles.
    fn runtime(&self) -> ParseRuntime {
        ParseRuntime::new(
            Arc::clone(&self.config),
            Arc::clone(&self.layout_engine),
            self.ocr_engine.as_ref().map(Arc::clone),
        )
    }
}
