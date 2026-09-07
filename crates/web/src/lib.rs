//! Browser-only ABI, ORT initialization, logging, and PDFium libc compatibility.
use docparse_config::{
    ExecutionProviderConfig, OutputConfig, RawConfig, ValidatedConfig,
};
use docparse_core::{
    DocParser, DocumentResult, JsonRenderer, MarkdownRenderer, TextRenderer,
};
use docparse_layout::ModelArtifacts;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        let _ = tracing::subscriber::set_global_default(BrowserLog);
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(
                &format!("DocParse WASM panic: {info}").into(),
            )
        }));
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
        let document = self
            .parser
            .parse_bytes(Arc::from(bytes))
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

/// Minimal subscriber that records meaningful event messages in the browser console.
struct BrowserLog;
impl tracing::Subscriber for BrowserLog {
    /// Limits release browser logging to lifecycle and error events.
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }
    /// Allocates a unique correlation identifier without browser-owned state.
    fn new_span(
        &self,
        _attributes: &tracing::span::Attributes<'_>,
    ) -> tracing::span::Id {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        tracing::span::Id::from_u64(NEXT.fetch_add(1, Ordering::Relaxed) as u64)
    }
    /// Leaves span fields unrecorded because event messages already contain diagnostic context.
    fn record(
        &self,
        _span: &tracing::span::Id,
        _values: &tracing::span::Record<'_>,
    ) {
    }
    /// Does not retain causal links between completed spans.
    fn record_follows_from(
        &self,
        _span: &tracing::span::Id,
        _follows: &tracing::span::Id,
    ) {
    }
    /// Emits the formatted message without dumping structured payloads.
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        if *event.metadata().level() == tracing::Level::ERROR {
            web_sys::console::error_1(&message.0.into());
        } else {
            web_sys::console::log_1(&message.0.into());
        }
    }
    /// Does not retain Worker-local span entry state.
    fn enter(&self, _span: &tracing::span::Id) {}
    /// Does not retain Worker-local span exit state.
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Captures only tracing's readable event message.
struct Message(String);
impl tracing::field::Visit for Message {
    /// Ignores non-message fields to avoid logging arbitrary payloads.
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn std::fmt::Debug,
    ) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// Supplies a process identity for PDFium's single-instance browser libc.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> i32 {
    1
}
/// Initializes a mutex in the deliberately single-threaded PDFium instance.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_init(
    _mutex: *mut u8,
    _attributes: *const u8,
) -> i32 {
    0
}
/// Serial actor execution provides mutual exclusion for this browser instance.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_lock(_mutex: *mut u8) -> i32 {
    0
}
/// Ends the actor-local critical section without emulating native threads.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_unlock(_mutex: *mut u8) -> i32 {
    0
}
/// Releases the single-threaded mutex placeholder without owning external resources.
#[allow(unsafe_code)] // Required C ABI exports; no Rust memory is dereferenced here.
#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_destroy(_mutex: *mut u8) -> i32 {
    0
}
