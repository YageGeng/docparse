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
    /// Formula recognition was enabled without its graph/tokenizer byte set.
    #[error(
        "formula recognition requires explicit model and tokenizer artifacts"
    )]
    MissingFormulaArtifacts,
    /// The selected formula graph or tokenizer failed initialization.
    #[error(transparent)]
    Formula(#[from] docparse_formula::FormulaError),
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
    /// Shared PDFium execution boundary; library callers keep the local provider.
    #[builder(default = Arc::new(crate::LocalPdfiumProvider))]
    pdfium_provider: Arc<dyn crate::PdfiumProvider>,
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    #[builder(default)]
    table_engine: Option<Arc<dyn crate::TableStructureEngine>>,
    #[builder(default)]
    formula_engine: Option<Arc<dyn docparse_formula::FormulaEngine>>,
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
#[derive(Clone, TypedBuilder)]
pub struct ParserArtifacts {
    pub layout: docparse_layout::ModelArtifacts,
    /// Required when table recovery is enabled and no table engine is injected.
    #[builder(default)]
    pub tsr: Option<docparse_tsr::TsrArtifacts>,
    /// Required for enabled built-in OCR when no external engine is injected.
    #[builder(default)]
    pub ocr: Option<docparse_ocr::OcrArtifacts>,
    /// Required when formula recognition is enabled without an injected recognizer.
    #[builder(default)]
    pub formula: Option<docparse_formula::FormulaArtifacts>,
    /// Alternative Texo encoder/decoder/tokenizer bytes; mutually exclusive with `formula`.
    #[builder(default)]
    pub texo_formula: Option<docparse_formula_texo::TexoArtifacts>,
}

impl From<docparse_layout::ModelArtifacts> for ParserArtifacts {
    /// Preserves the single-model convenience input for rules-only parsers.
    fn from(layout: docparse_layout::ModelArtifacts) -> Self {
        Self::builder().layout(layout).build()
    }
}

/// Consuming dependency-injection builder for one reusable parser instance.
#[derive(TypedBuilder)]
pub struct DocParserBuilder {
    /// Shared PDFium execution boundary; library callers keep the local provider.
    #[builder(default = Arc::new(crate::LocalPdfiumProvider))]
    pdfium_provider: Arc<dyn crate::PdfiumProvider>,
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
    formula_engine: Option<Arc<dyn docparse_formula::FormulaEngine>>,
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
    /// Injects a formula recognizer while retaining the configured batch and timeout policy.
    pub fn formula_engine(
        mut self,
        engine: Arc<dyn docparse_formula::FormulaEngine>,
    ) -> Self {
        self.formula_engine = Some(engine);
        self
    }
    /// Uses an externally owned PDFium provider without changing model ownership.
    pub fn pdfium_provider(
        mut self,
        provider: Arc<dyn crate::PdfiumProvider>,
    ) -> Self {
        self.pdfium_provider = provider;
        self
    }

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

    /// Loads the prevalidated formula selection, preserving injected engines and disabled recognition.
    async fn load_formula_engine(
        config: &Arc<ValidatedConfig>,
        injected: Option<Arc<dyn docparse_formula::FormulaEngine>>,
        pp_artifacts: Option<docparse_formula::FormulaArtifacts>,
        texo_artifacts: Option<docparse_formula_texo::TexoArtifacts>,
    ) -> Result<Option<Arc<dyn docparse_formula::FormulaEngine>>, DocParseError>
    {
        if let Some(engine) = injected {
            return Ok(Some(engine));
        }
        if !config.formula().inline_enabled && !config.formula().display_enabled
        {
            return Ok(None);
        }
        let engine: Arc<dyn docparse_formula::FormulaEngine> = match &config
            .formula()
            .engine
        {
            docparse_config::FormulaEngineConfig::Texo(_) => {
                Arc::new(match texo_artifacts {
                    Some(artifacts) => {
                        docparse_formula_texo::TexoEngine::from_artifacts(
                            Arc::clone(config),
                            artifacts,
                        )
                        .await?
                    }
                    None => {
                        docparse_formula_texo::TexoEngine::from_config(
                            Arc::clone(config),
                        )
                        .await?
                    }
                })
            }
            docparse_config::FormulaEngineConfig::Pp(_) => {
                Arc::new(match pp_artifacts {
                    Some(artifacts) => {
                        docparse_formula::PpFormulaNetEngine::from_artifacts(
                            Arc::clone(config),
                            artifacts,
                        )
                        .await?
                    }
                    None => {
                        docparse_formula::PpFormulaNetEngine::from_config(
                            Arc::clone(config),
                        )
                        .await?
                    }
                })
            }
            docparse_config::FormulaEngineConfig::Mineru(_) => Arc::new(
                docparse_formula_mineru::MineruEngine::try_from(
                    config.as_ref(),
                )
                .map_err(docparse_formula::FormulaError::from)?,
            ),
        };
        Ok(Some(engine))
    }

    /// Loads layout and enabled OCR/table models once, preserving explicitly injected engines.
    pub async fn build(self) -> Result<DocParser, DocParseError> {
        let config = self.config.ok_or(DocParseError::MissingConfiguration)?;
        let formula_enabled =
            config.formula().inline_enabled || config.formula().display_enabled;
        // Remote formula recognition consumes crops only; byte-backed parsers need no local formula artifacts.
        let local_formula_enabled = formula_enabled
            && !matches!(
                config.formula().engine,
                docparse_config::FormulaEngineConfig::Mineru(_)
            );
        if local_formula_enabled
            && self.formula_engine.is_none()
            && self.artifacts.as_ref().is_some_and(|artifacts| {
                artifacts.formula.is_none() && artifacts.texo_formula.is_none()
            })
        {
            tracing::error!(
                "parser artifact set is missing the enabled formula model/tokenizer"
            );
            return Err(DocParseError::MissingFormulaArtifacts);
        }
        if self.formula_engine.is_none()
            && local_formula_enabled
            && self.artifacts.as_ref().is_some_and(|artifacts| {
                artifacts.formula.is_some() && artifacts.texo_formula.is_some()
            })
        {
            tracing::error!(
                "parser artifact set contains two formula models; select PP-FormulaNet or Texo"
            );
            return Err(docparse_formula::FormulaError::Artifacts(
                "select either formula or texo_formula artifacts".into(),
            )
            .into());
        }
        if self.formula_engine.is_none()
            && local_formula_enabled
            && let Some(artifacts) = &self.artifacts
        {
            let selected_present = match config.formula().engine {
                docparse_config::FormulaEngineConfig::Pp(_) => {
                    artifacts.formula.is_some()
                }
                docparse_config::FormulaEngineConfig::Texo(_) => {
                    artifacts.texo_formula.is_some()
                }
                docparse_config::FormulaEngineConfig::Mineru(_) => true,
            };
            if !selected_present {
                tracing::error!(
                    "formula artifacts do not match the explicitly configured engine"
                );
                return Err(docparse_formula::FormulaError::Artifacts(
                    "formula artifacts do not match formula.engine.type".into(),
                )
                .into());
            }
        }
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
        let (
            layout_artifacts,
            table_artifacts,
            ocr_artifacts,
            formula_artifacts,
            texo_artifacts,
        ) = self.artifacts.map_or(
            (None, None, None, None, None),
            |artifacts| {
                (
                    Some(artifacts.layout),
                    artifacts.tsr,
                    artifacts.ocr,
                    artifacts.formula,
                    artifacts.texo_formula,
                )
            },
        );
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
        let formula_engine = Self::load_formula_engine(
            &config,
            self.formula_engine,
            formula_artifacts,
            texo_artifacts,
        )
        .await?;
        Ok(DocParser::with_engines()
            .pdfium_provider(self.pdfium_provider)
            .config(config)
            .layout_engine(layout_engine)
            .ocr_engine(ocr_engine)
            .table_engine(table_engine)
            .formula_engine(formula_engine)
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
                .formula_engine(self.formula_engine.as_ref().map(Arc::clone))
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
            .pdfium_provider(Arc::clone(&self.pdfium_provider))
            .config(Arc::clone(&self.config))
            .layout_engine(Arc::clone(&self.layout_engine))
            .ocr_engine(self.ocr_engine.as_ref().map(Arc::clone))
            .table_engine(self.table_engine.as_ref().map(Arc::clone))
            .formula_engine(self.formula_engine.as_ref().map(Arc::clone))
            .glyph_resolver(self.glyph_resolver.as_ref().map(Arc::clone))
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Model loading should not require a real layout graph when a caller injects its engine.
    struct UnusedLayout;
    impl LayoutEngine for UnusedLayout {
        /// Identifies the injected test dependency.
        fn name(&self) -> &str {
            "unused-layout"
        }
        /// Reports a fixed revision without loading weights.
        fn model_revision(&self) -> &str {
            "test"
        }
        /// Loading a parser must not invoke inference on this engine.
        fn detect(
            &self,
            _request: docparse_layout::LayoutRequest,
        ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
            '_,
            Result<
                Vec<docparse_layout::LayoutDetection>,
                docparse_layout::LayoutError,
            >,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    /// Enabled Texo byte artifacts must reach their own verifier, not PP-FormulaNet's verifier.
    #[tokio::test]
    async fn texo_artifacts_select_the_texo_loader() {
        let mut raw = docparse_config::RawConfig::default();
        raw.formula.engine = docparse_config::FormulaEngineConfig::Texo(
            docparse_config::TexoFormulaConfig::default(),
        );
        raw.ocr.policy = docparse_config::OcrPolicy::Disabled;
        raw.tsr.mode = crate::TableMode::RulesOnly;
        let empty: Arc<[u8]> = Arc::from([]);
        let artifacts = ParserArtifacts::builder()
            .layout(docparse_layout::ModelArtifacts {
                model: Arc::clone(&empty),
                config: Arc::clone(&empty),
                manifest: Arc::clone(&empty),
            })
            .texo_formula(Some(docparse_formula_texo::TexoArtifacts {
                encoder: Arc::clone(&empty),
                decoder: Arc::clone(&empty),
                tokenizer: empty,
            }))
            .build();
        let result = DocParser::builder()
            .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
            .layout_engine(Arc::new(UnusedLayout))
            .artifacts(artifacts)
            .build()
            .await;
        assert!(
            matches!(result, Err(DocParseError::Formula(docparse_formula::FormulaError::Artifacts(message))) if message.contains("Texo encoder_model.onnx"))
        );
    }

    /// An external formula engine needs neither model files nor formula bytes in an explicit artifact set.
    #[tokio::test]
    async fn mineru_loads_without_formula_artifacts() {
        for explicit in [false, true] {
            let mut raw = docparse_config::RawConfig::default();
            raw.formula.engine = docparse_config::FormulaEngineConfig::Mineru(
                docparse_config::MineruFormulaConfig::default(),
            );
            raw.ocr.policy = docparse_config::OcrPolicy::Disabled;
            raw.tsr.mode = crate::TableMode::RulesOnly;
            let mut builder = DocParser::builder()
                .config(Arc::new(
                    ValidatedConfig::try_from(raw).expect("config"),
                ))
                .layout_engine(Arc::new(UnusedLayout));
            if explicit {
                let empty: Arc<[u8]> = Arc::from([]);
                builder = builder.artifacts(
                    ParserArtifacts::builder()
                        .layout(docparse_layout::ModelArtifacts {
                            model: Arc::clone(&empty),
                            config: Arc::clone(&empty),
                            manifest: empty,
                        })
                        .build(),
                );
            }
            let parser = builder.build().await.expect("remote formula parser");
            assert_eq!(
                parser
                    .formula_engine
                    .as_ref()
                    .expect("formula engine")
                    .name(),
                "mineru-2.5-vllm"
            );
        }
    }

    /// Native configuration and explicit bytes both initialize the real Texo engine.
    #[tokio::test]
    #[ignore = "requires the pinned models/texo assets"]
    async fn loads_real_texo_from_files_and_bytes() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../models/texo");
        let mut raw = docparse_config::RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::Disabled;
        raw.tsr.mode = crate::TableMode::RulesOnly;
        raw.formula.engine = docparse_config::FormulaEngineConfig::Texo(
            docparse_config::TexoFormulaConfig::builder()
                .encoder_path(dir.join("encoder_model.onnx"))
                .decoder_path(dir.join("decoder_model_merged.onnx"))
                .tokenizer_path(dir.join("tokenizer.json"))
                .build(),
        );
        let config = Arc::new(ValidatedConfig::try_from(raw).expect("config"));
        for explicit in [false, true] {
            let mut builder = DocParser::builder()
                .config(Arc::clone(&config))
                .layout_engine(Arc::new(UnusedLayout));
            if explicit {
                let empty: Arc<[u8]> = Arc::from([]);
                builder = builder.artifacts(
                    ParserArtifacts::builder()
                        .layout(docparse_layout::ModelArtifacts {
                            model: Arc::clone(&empty),
                            config: Arc::clone(&empty),
                            manifest: empty,
                        })
                        .texo_formula(Some(
                            docparse_formula_texo::TexoArtifacts::try_from(match &config.formula().engine { docparse_config::FormulaEngineConfig::Texo(paths) => paths, _ => unreachable!("Texo config") })
                            .expect("assets"),
                        ))
                        .build(),
                );
            }
            let parser = builder.build().await.expect("Texo parser");
            assert!(
                parser
                    .formula_engine
                    .as_ref()
                    .expect("formula engine")
                    .name()
                    .starts_with("texo-transfer-onnx-")
            );
        }
    }
}
