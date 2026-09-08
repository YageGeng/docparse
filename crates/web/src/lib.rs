//! Browser-only parser ABI and ORT initialization.
mod libc;
mod logging;

use docparse_config::{
    ExecutionProviderConfig, OutputConfig, RawConfig, ValidatedConfig,
};
use docparse_core::{
    DocParser, DocumentResult, JsonRenderer, MarkdownRenderer, ParseObserver,
    ParseProgress, TextRenderer,
};
use docparse_layout::ModelArtifacts;
use serde::Serialize;
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
        let value = js_sys::Error::new(&error.message);
        let _ = js_sys::Reflect::set(
            &value,
            &JsValue::from_str("code"),
            &JsValue::from_str(error.code),
        );
        value.into()
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
    ) -> Result<WebParser, JsValue> {
        logging::init();
        let mut raw: RawConfig = serde_wasm_bindgen::from_value(options)
            .map_err(|error| WebError::value("InvalidConfig", error))?;
        raw.layout.execution_provider = if webgpu {
            ExecutionProviderConfig::WebGpu
        } else {
            ExecutionProviderConfig::Cpu
        };
        let output = raw.output.clone();
        let validated = ValidatedConfig::try_from(raw)
            .map_err(|error| WebError::value("InvalidConfig", error))?;
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
        // The host uses a dedicated Worker; ORT need not create another proxy or thread pool.
        let runtime = js_sys::Reflect::get(&js_sys::global(), &"ort".into())
            .map_err(|error| {
                WebError::value(
                    "RuntimeInitializationFailed",
                    format!("{error:?}"),
                )
            })?;
        let env =
            js_sys::Reflect::get(&runtime, &"env".into()).map_err(|error| {
                WebError::value(
                    "RuntimeInitializationFailed",
                    format!("{error:?}"),
                )
            })?;
        let wasm =
            js_sys::Reflect::get(&env, &"wasm".into()).map_err(|error| {
                WebError::value(
                    "RuntimeInitializationFailed",
                    format!("{error:?}"),
                )
            })?;
        js_sys::Reflect::set(&wasm, &"numThreads".into(), &1.into()).map_err(
            |error| {
                WebError::value(
                    "RuntimeInitializationFailed",
                    format!("{error:?}"),
                )
            },
        )?;
        js_sys::Reflect::set(&wasm, &"proxy".into(), &false.into()).map_err(
            |error| {
                WebError::value(
                    "RuntimeInitializationFailed",
                    format!("{error:?}"),
                )
            },
        )?;
        let artifacts = ModelArtifacts {
            model: Arc::from(model),
            config: Arc::from(config),
            manifest: Arc::from(manifest),
        };
        let parser = DocParser::from_artifacts(validated, artifacts)
            .await
            .map_err(|error| {
                let code = match &error {
                    docparse_core::DocParseError::Layout(docparse_layout::LayoutError::ExecutionProviderUnavailable { .. }) => "ExecutionProviderUnavailable",
                    docparse_core::DocParseError::Layout(docparse_layout::LayoutError::Ort { .. }) if webgpu => "ExecutionProviderInitializationFailed",
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
        self.parse_with_observer(bytes, None, None).await
    }

    /// Relays progress and borrowed PDFium rasters through Worker-local callbacks.
    pub async fn parse_with_observer(
        &self,
        bytes: Vec<u8>,
        progress: Option<js_sys::Function>,
        page_image: Option<js_sys::Function>,
    ) -> Result<JsValue, JsValue> {
        let observer = BrowserObserver {
            progress,
            page_image,
        };
        let document = self
            .parser
            .parse_bytes_with_observer(Arc::from(bytes), &observer)
            .await
            .map_err(|error| WebError::value("DocumentFailed", error))?;
        document
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|error| WebError::value("SerializationFailed", error))
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

/// Callbacks remain local to the dedicated Worker and never enter native thread bounds.
struct BrowserObserver {
    progress: Option<js_sys::Function>,
    page_image: Option<js_sys::Function>,
}

impl ParseObserver for BrowserObserver {
    /// Serializes only a small progress record at each real pipeline boundary.
    fn on_progress(&self, progress: ParseProgress) {
        if let Some(callback) = &self.progress {
            let result = progress
                .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
                .map_err(JsValue::from)
                .and_then(|value| callback.call1(&JsValue::NULL, &value));
            if let Err(error) = result {
                web_sys::console::error_1(&error);
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
            let pixels = js_sys::Uint8Array::from(image.data().as_ref());
            if let Err(error) = callback.call4(
                &JsValue::NULL,
                &page_number.into(),
                &image.width().into(),
                &image.height().into(),
                &pixels.into(),
            ) {
                web_sys::console::error_1(&error);
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
