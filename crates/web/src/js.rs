//! Safe, Worker-local JavaScript operations shared by the parser and table bridge.
// js-sys catches exceptions for property access and function calls. Keep those
// Results intact, and copy pixels instead of exposing views into Rust memory.
use wasm_bindgen::{JsCast, JsValue};

pub(super) use js_sys::Function;

/// Checked property access and owned exception messages for JavaScript values.
pub(super) trait ValueExt {
    /// Reads a named property, preserving exceptions from getters and proxies.
    fn property(&self, name: &str) -> Result<JsValue, JsValue>;
    /// Rejects both thrown setters and Reflect.set returning false.
    fn set_property(&self, name: &str, value: &JsValue) -> Result<(), JsValue>;
    /// Converts arbitrary exceptions without letting a throwing message getter escape.
    fn exception_message(&self) -> String;
}

impl ValueExt for JsValue {
    /// Preserves the safe js-sys exception boundary for property reads.
    fn property(&self, name: &str) -> Result<JsValue, JsValue> {
        js_sys::Reflect::get(self, &JsValue::from_str(name))
    }

    /// Treats a non-writable property as a failed operation instead of silent success.
    fn set_property(&self, name: &str, value: &JsValue) -> Result<(), JsValue> {
        if js_sys::Reflect::set(self, &JsValue::from_str(name), value)? {
            Ok(())
        } else {
            Err(JsValue::from_str(&format!(
                "cannot write JavaScript property {name}"
            )))
        }
    }

    /// Owns the message before it crosses into core error types or tracing.
    fn exception_message(&self) -> String {
        self.property("message")
            .ok()
            .and_then(|message| message.as_string())
            .or_else(|| self.as_string())
            .unwrap_or_else(|| "JavaScript operation failed".to_owned())
    }
}

/// Callback invocation keeps only JS-owned values across asynchronous work.
pub(super) trait FunctionExt {
    /// Invokes a callback with a null receiver and catches synchronous exceptions.
    fn invoke(&self, arguments: &[JsValue]) -> Result<JsValue, JsValue>;
    /// Accepts plain values, promises and thenables, preserving asynchronous rejection.
    async fn invoke_async(
        &self,
        argument: &JsValue,
    ) -> Result<JsValue, JsValue>;
}

impl FunctionExt for Function {
    /// Copies the argument handles into a JS array before calling caller-owned code.
    fn invoke(&self, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
        self.apply(&JsValue::NULL, &arguments.iter().collect::<js_sys::Array>())
    }

    /// Resolves the callback result without borrowing a WASM pixel buffer across await.
    async fn invoke_async(
        &self,
        argument: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let result = self.invoke(std::slice::from_ref(argument))?;
        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&result))
            .await
    }
}

/// Validated access to the per-call Worker callback object.
pub(super) struct Callbacks(JsValue);

impl From<JsValue> for Callbacks {
    /// Retains an optional callback object without allocating an empty replacement.
    fn from(value: JsValue) -> Self {
        Self(value)
    }
}

impl Callbacks {
    /// Checks callback properties before exposing them to async Rust extension points.
    pub fn function(&self, name: &str) -> Result<Option<Function>, JsValue> {
        if self.0.is_null() || self.0.is_undefined() {
            return Ok(None);
        }
        let value = self.0.property(name).map_err(|error| {
            super::WebError::value(
                "InvalidOptions",
                format!(
                    "cannot read callback {name}: {}",
                    error.exception_message()
                ),
            )
        })?;
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }
        value.dyn_into().map(Some).map_err(|_value| {
            super::WebError::value(
                "InvalidOptions",
                format!("callback {name} must be a function"),
            )
        })
    }
}

/// Applies the single-Worker runtime settings before either model creates a session.
pub(super) fn configure_runtime() -> Result<(), JsValue> {
    let global: JsValue = js_sys::global().into();
    let wasm = global.property("ort")?.property("env")?.property("wasm")?;
    wasm.set_property("numThreads", &JsValue::from_f64(1.0))?;
    wasm.set_property("proxy", &JsValue::FALSE)
}

/// Creates a JS error carrying the stable protocol category used by the SDK.
pub(super) fn error(code: &str, message: &str) -> JsValue {
    let value: JsValue = js_sys::Error::new(message).into();
    if let Err(error) = value.set_property("code", &JsValue::from_str(code)) {
        console(&JsValue::from_str(&error.exception_message()), true);
    }
    value
}

/// Copies RGB bytes into independent JS storage that survives Rust buffer release and heap growth.
pub(super) fn copy_pixels(pixels: &[u8]) -> JsValue {
    js_sys::Uint8Array::from(pixels).into()
}

/// Reports browser diagnostics without recursing through the tracing subscriber.
pub(super) fn console(value: &JsValue, error: bool) {
    if error {
        web_sys::console::error_1(value);
    } else {
        web_sys::console::log_1(value);
    }
}
