use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
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
    /// Byte-based construction requires every enabled model to be supplied explicitly.
    #[error("table recovery requires explicit TSR model artifacts")]
    MissingTsrArtifacts,
    /// Enabled built-in OCR cannot load models implicitly in byte-artifact mode.
    #[error(
        "OCR requires explicit detector, recognizer and enabled orientation artifacts"
    )]
    MissingOcrArtifacts,
    #[error(transparent)]
    BuiltinOcr(#[from] docparse_ocr::OcrError),
    /// The default table model failed to initialize.
    #[error(transparent)]
    Tsr(#[from] docparse_tsr::TsrError),
    /// Per-call table policy or provider configuration is invalid.
    #[error(transparent)]
    TableStructure(#[from] crate::TableStructureError),
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
        match source {
            ParseRuntimeError::Table(error) => Self::TableStructure(error),
            source => Self::Runtime {
                source: Box::new(source),
            },
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

/// Per-call table policy, optional structure provider, and serial progress observations.
#[derive(Default, Clone, TypedBuilder)]
pub struct ParseOptions<'a> {
    /// Missing values inherit the parser instance's table configuration.
    #[builder(default, setter(strip_option))]
    pub table: Option<crate::TableOptions>,
    #[builder(default)]
    pub table_engine: Option<Arc<dyn crate::TableStructureEngine>>,
    #[builder(default)]
    pub observer: Option<&'a dyn crate::ParseObserver>,
}

impl ParseOptions<'_> {
    /// Combines per-call overrides with the parser's built-in provider without allocating a second model.
    pub(crate) fn table_runtime(
        &self,
        config: &ValidatedConfig,
        default_engine: Option<&Arc<dyn crate::TableStructureEngine>>,
    ) -> Result<Arc<crate::runtime::TableRuntime>, crate::TableStructureError>
    {
        let options = self
            .table
            .clone()
            .unwrap_or_else(|| crate::TableOptions::from(config.tsr()));
        let engine = self
            .table_engine
            .as_ref()
            .or(default_engine)
            .map(Arc::clone);
        crate::runtime::TableRuntime::shared(options, engine)
    }
}

/// Public reusable parser with immutable shared configuration and engines.
#[derive(Clone, TypedBuilder)]
#[builder(builder_method(name = with_engines, vis = "pub(crate)"), builder_type(name = ParserAssembly, vis = "pub(crate)"))]
pub struct DocParser {
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    #[builder(default)]
    table_engine: Option<Arc<dyn crate::TableStructureEngine>>,
    /// Shared outline recovery is consulted only after deterministic font decoding fails.
    #[builder(default)]
    glyph_resolver: Option<Arc<dyn crate::GlyphResolver>>,
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

/// Explicit model bytes for filesystem-independent parser construction on native and Web.
#[derive(Clone)]
pub struct ParserArtifacts {
    pub layout: docparse_layout::ModelArtifacts,
    /// Required when table recovery is enabled and no table engine is injected.
    pub tsr: Option<docparse_layout::ModelArtifacts>,
    /// Required for enabled built-in OCR when no external engine is injected.
    pub ocr: Option<docparse_ocr::OcrArtifacts>,
}

impl From<docparse_layout::ModelArtifacts> for ParserArtifacts {
    /// Preserves the single-model convenience input for rules-only parsers.
    fn from(layout: docparse_layout::ModelArtifacts) -> Self {
        Self {
            layout,
            tsr: None,
            ocr: None,
        }
    }
}

/// Consuming dependency-injection builder for one reusable parser instance.
#[derive(TypedBuilder)]
pub struct DocParserBuilder {
    #[builder(default)]
    artifacts: Option<ParserArtifacts>,
    #[builder(default)]
    config: Option<Arc<ValidatedConfig>>,
    #[builder(default)]
    layout_engine: Option<Arc<dyn LayoutEngine>>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    #[builder(default)]
    table_engine: Option<Arc<dyn crate::TableStructureEngine>>,
    #[builder(default)]
    glyph_resolver: Option<Arc<dyn crate::GlyphResolver>>,
}

impl Default for DocParserBuilder {
    /// Starts with no injected engines; build resolves configured defaults.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl DocParserBuilder {
    /// Injects outline recovery for fonts without usable names or embedded Unicode mappings.
    pub fn glyph_resolver(
        mut self,
        resolver: Arc<dyn crate::GlyphResolver>,
    ) -> Self {
        self.glyph_resolver = Some(resolver);
        self
    }

    /// Supplies owned bytes; enabled models never fall back to filesystem loading in this mode.
    pub fn artifacts(mut self, artifacts: impl Into<ParserArtifacts>) -> Self {
        self.artifacts = Some(artifacts.into());
        self
    }

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

    /// Injects a table structure engine and bypasses built-in TSR artifact loading.
    pub fn table_engine(
        mut self,
        engine: Arc<dyn crate::TableStructureEngine>,
    ) -> Self {
        self.table_engine = Some(engine);
        self
    }

    /// Loads layout and enabled OCR/table models once, preserving explicitly injected engines.
    pub async fn build(self) -> Result<DocParser, DocParseError> {
        let config = self.config.ok_or(DocParseError::MissingConfiguration)?;
        let table_enabled = config.tsr().mode != crate::TableMode::RulesOnly;
        let ocr_enabled =
            config.ocr().policy != docparse_config::OcrPolicy::Disabled;
        if ocr_enabled
            && self.ocr_engine.is_none()
            && self
                .artifacts
                .as_ref()
                .is_some_and(|artifacts| artifacts.ocr.is_none())
        {
            return Err(DocParseError::MissingOcrArtifacts);
        }
        if self
            .artifacts
            .as_ref()
            .is_some_and(|artifacts| artifacts.tsr.is_none())
            && table_enabled
            && self.table_engine.is_none()
        {
            tracing::error!(
                "parser artifact set is missing the enabled TSR model"
            );
            return Err(DocParseError::MissingTsrArtifacts);
        }
        let (layout_artifacts, table_artifacts, ocr_artifacts) =
            self.artifacts.map_or((None, None, None), |artifacts| {
                (Some(artifacts.layout), artifacts.tsr, artifacts.ocr)
            });
        let layout_engine: Arc<dyn LayoutEngine> =
            match (self.layout_engine, layout_artifacts) {
                (Some(engine), _) => engine,
                (None, Some(artifacts)) => Arc::new(
                    PpDocLayoutV3Engine::from_artifacts(
                        Arc::clone(&config),
                        artifacts,
                    )
                    .await?,
                ),
                (None, None) => Arc::new(
                    PpDocLayoutV3Engine::from_config(Arc::clone(&config))
                        .await?,
                ),
            };
        let table_engine =
            match (self.table_engine, table_enabled, table_artifacts) {
                (Some(engine), _, _) => Some(engine),
                (None, true, Some(artifacts)) => Some(Arc::new(
                    docparse_tsr::SlanetPlusEngine::from_artifacts(
                        Arc::clone(&config),
                        artifacts,
                    )
                    .await?,
                )
                    as Arc<dyn crate::TableStructureEngine>),
                (None, true, None) => Some(Arc::new(
                    docparse_tsr::SlanetPlusEngine::from_config(Arc::clone(
                        &config,
                    ))
                    .await?,
                )
                    as Arc<dyn crate::TableStructureEngine>),
                (None, false, _) => None,
            };
        // Keep injected engines authoritative and initialize built-in OCR only when explicitly enabled.
        let ocr_engine = match (self.ocr_engine, ocr_enabled, ocr_artifacts) {
            (Some(engine), _, _) => Some(engine),
            (None, true, Some(artifacts)) => Some(Arc::new(
                docparse_ocr::PaddleOcrEngine::from_artifacts(
                    Arc::clone(&config),
                    artifacts,
                )
                .await?,
            )
                as Arc<dyn OcrEngine>),
            (None, true, None) => Some(Arc::new(
                docparse_ocr::PaddleOcrEngine::from_config(Arc::clone(&config))
                    .await?,
            ) as Arc<dyn OcrEngine>),
            (None, false, _) => None,
        };
        Ok(DocParser::with_engines()
            .config(config)
            .layout_engine(layout_engine)
            .ocr_engine(ocr_engine)
            .table_engine(table_engine)
            // Resolve the optional native database once per parser, sharing its shard cache across documents.
            .glyph_resolver(
                self.glyph_resolver
                    .or_else(crate::wasm_compat::default_glyph_resolver),
            )
            .build())
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

    /// Builds from explicit layout and enabled TSR bytes without accessing model paths.
    pub async fn from_artifacts(
        config: ValidatedConfig,
        artifacts: impl Into<ParserArtifacts>,
    ) -> Result<Self, DocParseError> {
        Self::builder()
            .config(Arc::new(config))
            .artifacts(artifacts)
            .build()
            .await
    }

    /// Parses one shared in-memory PDF without copying its bytes.
    pub async fn parse_bytes(
        &self,
        bytes: Arc<[u8]>,
    ) -> Result<DocumentResult, DocParseError> {
        self.parse_bytes_with_options(bytes, ParseOptions::default())
            .await
    }

    /// Parses shared bytes with per-call table recovery and an optional external engine.
    pub async fn parse_bytes_with_options(
        &self,
        bytes: Arc<[u8]>,
        options: ParseOptions<'_>,
    ) -> Result<DocumentResult, DocParseError> {
        self.runtime()
            .parse_document_with_options(PdfInput::Bytes(bytes), options)
            .await
            .map_err(DocParseError::from)
    }

    /// Parses shared PDF bytes while reporting real progress and the inference page rasters.
    pub async fn parse_bytes_with_observer(
        &self,
        bytes: Arc<[u8]>,
        observer: &dyn crate::ParseObserver,
    ) -> Result<DocumentResult, DocParseError> {
        self.parse_bytes_with_options(
            bytes,
            ParseOptions::builder().observer(Some(observer)).build(),
        )
        .await
    }

    /// Parses one already extracted and rendered page with a one-page context.
    pub async fn parse_page(
        &self,
        input: PageInput,
    ) -> Result<PageResult, DocParseError> {
        self.parse_page_with_options(input, ParseOptions::default())
            .await
    }

    /// Parses one owned page using the same per-call table policy as a full document.
    pub async fn parse_page_with_options(
        &self,
        mut input: PageInput,
        options: ParseOptions<'_>,
    ) -> Result<PageResult, DocParseError> {
        let observer = options.observer;
        let (collector, mut timing_receiver) =
            docparse_layout::timing::Timings::channel();
        let timings = if observer.is_some() {
            collector
        } else {
            docparse_layout::timing::Timings::default()
        };
        let tables =
            options.table_runtime(&self.config, self.table_engine.as_ref())?;
        let source_page_number = input.extracted.page_number;
        crate::watermark::classify(
            std::iter::once(&mut input.extracted),
            self.config.fusion(),
        )
        .map_err(|source| {
            tracing::error!(
                "watermark classification failed on standalone page {}: {}",
                source_page_number,
                source
            );
            DocParseError::ParsePage {
                source: Box::new(source),
            }
        })?;
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
        if let Some(observer) = observer {
            observer.on_progress(crate::ParseProgress::Analyzing {
                completed: 0,
                total: 1,
            });
            observer.on_page_image(input.extracted.page_number, &input.image);
        }
        let rendered = RenderedPage::builder()
            .page_number(input.extracted.page_number)
            .image(input.image)
            .transform(input.transform)
            .build();
        let result = analyze_rendered_page(
            crate::runtime::PageAnalysisInput::builder()
                .config(Arc::clone(&self.config))
                .layout_engine(Arc::clone(&self.layout_engine))
                .ocr_engine(self.ocr_engine.as_ref().map(Arc::clone))
                .context(context)
                .extracted(input.extracted)
                .rendered(rendered)
                .timings(timings)
                .tables(tables)
                .build(),
        )
        .await
        .map_err(|source| DocParseError::ParsePage {
            source: Box::new(source),
        });
        if let Some(observer) = observer {
            while let Ok(timing) = timing_receiver.try_recv() {
                observer.on_timing(timing);
            }
            if result.is_ok() {
                observer.on_progress(crate::ParseProgress::Analyzing {
                    completed: 1,
                    total: 1,
                });
                observer
                    .on_progress(crate::ParseProgress::Complete { total: 1 });
            }
        }
        result
    }

    /// Creates one short-lived runtime facade that clones only shared ownership handles.
    pub(crate) fn runtime(&self) -> ParseRuntime {
        ParseRuntime::builder()
            .config(Arc::clone(&self.config))
            .layout_engine(Arc::clone(&self.layout_engine))
            .ocr_engine(self.ocr_engine.as_ref().map(Arc::clone))
            .table_engine(self.table_engine.as_ref().map(Arc::clone))
            .glyph_resolver(self.glyph_resolver.as_ref().map(Arc::clone))
            .build()
    }
}
