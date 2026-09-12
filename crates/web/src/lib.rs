//! Browser-only parser ABI and ORT initialization.
#![deny(unsafe_code)]

mod js;
mod libc;
mod logging;
mod table;

use js::{FunctionExt, ValueExt};

use docparse_config::{
    ExecutionProviderConfig, OutputConfig, RawConfig, ValidatedConfig,
};
use docparse_core::{
    DocParser, DocumentResult, JsonRenderer, MarkdownRenderer, ParseObserver,
    ParseProgress, TextRenderer,
};
use docparse_layout::ModelArtifacts;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wasm_bindgen::prelude::*;

/// A browser-safe error with a stable machine-readable category.
struct WebError {
    code: &'static str,
    message: String,
}

impl WebError {
    /// Serializes a failure without retaining a JavaScript handle in Rust error chains.
    fn value(code: &'static str, error: impl std::fmt::Display) -> JsValue {
        let error = Self {
            code,
            message: error.to_string(),
        };
        tracing::error!("{}: {}", error.code, error.message);
        js::error(error.code, &error.message)
    }
}

/// Owned model bytes shared by table and OCR artifact boundaries.
#[derive(Deserialize)]
struct ArtifactBytes {
    model: Vec<u8>,
    config: Vec<u8>,
    manifest: Vec<u8>,
}

impl From<ArtifactBytes> for ModelArtifacts {
    /// Transfers deserialized model buffers into the shared immutable artifact container.
    fn from(value: ArtifactBytes) -> Self {
        Self {
            model: Arc::from(value.model),
            config: Arc::from(value.config),
            manifest: Arc::from(value.manifest),
        }
    }
}

/// Optional model families share one ABI argument without exposing filesystem paths.
#[derive(Deserialize)]
struct AuxiliaryArtifacts {
    tsr: Option<ArtifactBytes>,
    ocr: Option<OcrArtifactBytes>,
}

/// The three independently verified PaddleOCR networks supplied by the Worker.
#[derive(Deserialize)]
struct OcrArtifactBytes {
    detection: ArtifactBytes,
    recognition: ArtifactBytes,
    orientation: Option<ArtifactBytes>,
}

impl From<OcrArtifactBytes> for docparse_core::OcrArtifacts {
    /// Moves buffers into the native/browser-neutral OCR artifact contract.
    fn from(value: OcrArtifactBytes) -> Self {
        Self {
            detection: value.detection.into(),
            recognition: value.recognition.into(),
            orientation: value.orientation.map(Into::into),
        }
    }
}

/// The parser owns a model session for its enclosing Worker's lifetime.
#[wasm_bindgen]
pub struct WebParser {
    parser: DocParser,
    output: OutputConfig,
}

#[wasm_bindgen]
impl WebParser {
    /// Initializes the fixed runtime and constructs the shared parser from owned artifacts.
    pub async fn create(
        model: Vec<u8>,
        config: Vec<u8>,
        manifest: Vec<u8>,
        options: JsValue,
        runtime_base: String,
        webgpu: bool,
        auxiliary_artifacts: JsValue,
    ) -> Result<WebParser, JsValue> {
        logging::init();
        let mut raw: RawConfig = serde_wasm_bindgen::from_value(options)
            .map_err(|error| WebError::value("InvalidConfig", error))?;
        raw.layout.execution_provider = if webgpu {
            ExecutionProviderConfig::WebGpu
        } else {
            ExecutionProviderConfig::Cpu
        };
        raw.tsr.execution_provider = raw.layout.execution_provider;
        // All model families use the selected Worker backend and shared GPU serialization.
        raw.ocr.execution_provider = raw.layout.execution_provider;
        let output = raw.output.clone();
        let validated = ValidatedConfig::try_from(raw)
            .map_err(|error| WebError::value("InvalidConfig", error))?;
        let auxiliary: AuxiliaryArtifacts = serde_wasm_bindgen::from_value(
            auxiliary_artifacts,
        )
        .map_err(|error| WebError::value("InvalidModelArtifacts", error))?;
        let table_artifacts =
            if validated.tsr().mode == docparse_core::TableMode::RulesOnly {
                None
            } else {
                auxiliary.tsr.map(ModelArtifacts::from)
            };
        let ocr_artifacts =
            if validated.ocr().policy == docparse_config::OcrPolicy::Disabled {
                None
            } else {
                auxiliary.ocr.map(docparse_core::OcrArtifacts::from)
            };
        let script = if webgpu {
            "ort.webgpu.min.mjs"
        } else {
            "ort.wasm.min.mjs"
        };
        let api = ort_web::api(
            ort_web::Dist::new(runtime_base).with_script_name(script),
        )
        .await
        .map_err(|error| {
            WebError::value("RuntimeInitializationFailed", error)
        })?;
        ort::set_api(api);
        ort::init().with_telemetry(false).commit();
        js::configure_runtime().map_err(|error| {
            WebError::value(
                "RuntimeInitializationFailed",
                error.exception_message(),
            )
        })?;
        let artifacts = ModelArtifacts {
            model: Arc::from(model),
            config: Arc::from(config),
            manifest: Arc::from(manifest),
        };
        let parser = DocParser::from_artifacts(
            validated,
            docparse_core::ParserArtifacts {
                layout: artifacts,
                tsr: table_artifacts,
                ocr: ocr_artifacts,
            },
        )
        .await
        .map_err(|error| {
            use docparse_core::DocParseError;
            use docparse_layout::LayoutError;
            use docparse_ocr::OcrError;
            use docparse_tsr::TsrError;
            let code = match &error {
                DocParseError::Layout(
                    LayoutError::ExecutionProviderUnavailable { .. },
                )
                | DocParseError::Tsr(TsrError::Backend(
                    LayoutError::ExecutionProviderUnavailable { .. },
                ))
                | DocParseError::BuiltinOcr(OcrError::Backend(
                    LayoutError::ExecutionProviderUnavailable { .. },
                )) => "ExecutionProviderUnavailable",
                DocParseError::Layout(LayoutError::Ort { .. })
                | DocParseError::Tsr(
                    TsrError::Backend(LayoutError::Ort { .. })
                    | TsrError::Ort(_),
                )
                | DocParseError::BuiltinOcr(
                    OcrError::Backend(LayoutError::Ort { .. })
                    | OcrError::Runtime(_),
                ) if webgpu => "ExecutionProviderInitializationFailed",
                DocParseError::Tsr(_) => "TableModelInitializationFailed",
                DocParseError::MissingTsrArtifacts => "TableArtifactsRequired",
                DocParseError::MissingOcrArtifacts => "OcrArtifactsRequired",
                DocParseError::BuiltinOcr(_) => "OcrModelInitializationFailed",
                _ => "ModelInitializationFailed",
            };
            WebError::value(code, error)
        })?;
        tracing::info!(
            "initialized browser parser with provider {}",
            if webgpu { "webgpu" } else { "wasm" }
        );
        Ok(Self { parser, output })
    }

    /// Runs the actual PDFium, model and fusion pipeline without exposing runtime handles.
    pub async fn parse(&self, bytes: Vec<u8>) -> Result<JsValue, JsValue> {
        self.parse_with_observer(bytes, None, None, None).await
    }

    /// Relays progress and borrowed PDFium rasters through Worker-local callbacks.
    pub async fn parse_with_observer(
        &self,
        bytes: Vec<u8>,
        progress: Option<js::Function>,
        page_image: Option<js::Function>,
        timing: Option<js::Function>,
    ) -> Result<JsValue, JsValue> {
        let observer = BrowserObserver {
            progress,
            page_image,
            timing,
        };
        self.parse_observed(bytes, None, None, observer).await
    }

    /// Runs table policy and an optional promise broker while preserving the legacy observer ABI.
    pub async fn parse_with_options(
        &self,
        bytes: Vec<u8>,
        options: JsValue,
        callbacks: JsValue,
    ) -> Result<JsValue, JsValue> {
        let options = if options.is_null() || options.is_undefined() {
            None
        } else {
            Some(
                serde_wasm_bindgen::from_value(options)
                    .map_err(|e| WebError::value("InvalidTableOptions", e))?,
            )
        };
        let callbacks = js::Callbacks::from(callbacks);
        let observer = BrowserObserver {
            progress: callbacks.function("progress")?,
            page_image: callbacks.function("page_image")?,
            timing: callbacks.function("timing")?,
        };
        let engine = match (
            callbacks.function("table_request")?,
            callbacks.function("table_cancel")?,
        ) {
            (None, None) => None,
            (Some(request), Some(cancel)) => {
                Some(table::BrowserTableEngine::shared(request, cancel))
            }
            _ => {
                return Err(WebError::value(
                    "InvalidTableOptions",
                    "table request and cancellation callbacks must be provided together",
                ));
            }
        };
        self.parse_observed(bytes, options, engine, observer).await
    }

    /// Applies the same native renderers to a validated canonical document.
    pub fn render(
        &self,
        document: JsValue,
        format: &str,
    ) -> Result<String, JsValue> {
        let document: DocumentResult = serde_wasm_bindgen::from_value(document)
            .map_err(|error| WebError::value("InvalidDocument", error))?;
        docparse_core::ResultValidator::validate(&document)
            .map_err(|error| WebError::value("InvalidDocument", error))?;
        match format {
            "json" => JsonRenderer::render_with_config(&document, &self.output)
                .map_err(|error| WebError::value("RenderFailed", error)),
            "text" => Ok(TextRenderer::new(
                docparse_core::RenderView::Semantic,
                self.output.formula_placeholder.clone(),
            )
            .render(&document)),
            "markdown" => Ok(MarkdownRenderer::new(
                docparse_core::RenderView::Semantic,
                self.output.formula_placeholder.clone(),
            )
            .render(&document)),
            _ => Err(WebError::value(
                "InvalidFormat",
                "expected json, text or markdown",
            )),
        }
    }
}

impl WebParser {
    /// Shares serialization and observations across the old and option-bearing parser entry points.
    async fn parse_observed(
        &self,
        bytes: Vec<u8>,
        options: Option<docparse_core::TableOptions>,
        engine: Option<Arc<dyn docparse_core::TableStructureEngine>>,
        observer: BrowserObserver,
    ) -> Result<JsValue, JsValue> {
        let document = self
            .parser
            .parse_bytes_with_options(
                Arc::from(bytes),
                docparse_core::ParseOptions {
                    table: options,
                    table_engine: engine,
                    observer: Some(&observer),
                },
            )
            .await
            .map_err(|error| {
                let code = match &error {
                    docparse_core::DocParseError::TableStructure(e) => e.code(),
                    _ => "DocumentFailed",
                };
                WebError::value(code, error)
            })?;
        let (timings, mut receiver) =
            docparse_layout::timing::Timings::channel();
        let timer = timings
            .start(docparse_layout::timing::TimingStage::ResultSerialize);
        let result = document
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| WebError::value("SerializationFailed", error));
        drop(timer);
        if let Ok(timing) = receiver.try_recv() {
            observer.on_timing(timing);
        }
        result
    }
}

/// Callbacks remain local to the dedicated Worker and never enter native thread bounds.
struct BrowserObserver {
    progress: Option<js::Function>,
    page_image: Option<js::Function>,
    timing: Option<js::Function>,
}

impl ParseObserver for BrowserObserver {
    /// Reports small timing records without adding volatile fields to canonical results.
    fn on_timing(&self, timing: docparse_core::Timing) {
        if let Some(callback) = &self.timing {
            let result = timing
                .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
                .map_err(JsValue::from)
                .and_then(|value| callback.invoke(&[value]));
            if let Err(error) = result {
                js::console(&error, true);
            }
        }
    }

    /// Serializes only a small progress record at each real pipeline boundary.
    fn on_progress(&self, progress: ParseProgress) {
        if let Some(callback) = &self.progress {
            let result = progress
                .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
                .map_err(JsValue::from)
                .and_then(|value| callback.invoke(&[value]));
            if let Err(error) = result {
                js::console(&error, true);
            }
        }
    }

    /// Copies RGB pixels out of WASM once so asynchronous PNG encoding cannot outlive a Rust borrow.
    fn on_page_image(
        &self,
        page_number: u32,
        image: &docparse_layout::PageImage,
    ) {
        if let Some(callback) = &self.page_image {
            let pixels = js::copy_pixels(image.data().as_ref());
            if let Err(error) = callback.invoke(&[
                page_number.into(),
                image.width().into(),
                image.height().into(),
                pixels,
            ]) {
                js::console(&error, true);
            }
        }
    }
}

/// Exposes native business defaults with explicit single-Worker concurrency defaults.
#[wasm_bindgen]
pub fn default_config() -> Result<JsValue, JsValue> {
    let mut raw = RawConfig::default();
    raw.layout.session_pool_size = 1;
    raw.runtime.page_concurrency = 1;
    raw.runtime.render_queue_capacity = 1;
    raw.runtime.blocking_task_limit = 1;
    raw.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|error| WebError::value("SerializationFailed", error))
}
