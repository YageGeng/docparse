//! Worker-local bridge to caller-owned external table structure callbacks.
use std::sync::Arc;

use docparse_core::{
    BlockId, TableStructureEngine, TableStructureError, TsrRequestReason,
    TsrTableInput, TsrTableRequest,
};
use docparse_layout::{AffineTransform, Bbox};
use serde::Serialize;
use typed_builder::TypedBuilder;
use wasm_bindgen::{JsCast, prelude::*};

/// Metadata is serialized separately from an owned pixel copy.
#[derive(Serialize, TypedBuilder)]
struct CropMetadata<'a> {
    request_id: &'a str,
    page_number: u32,
    block_id: &'a BlockId,
    crop_bbox: Bbox,
    crop_to_viewport: AffineTransform,
    reason: &'a TsrRequestReason,
    width: u32,
    height: u32,
}

/// The JS promise broker lives inside the Worker, never in serialized parse options.
pub(super) struct BrowserTableEngine {
    request: js_sys::Function,
    cancel: js_sys::Function,
}

impl BrowserTableEngine {
    /// Owns both halves of the broker so dropping a pending Rust future also cancels its JS request.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "the core API uses Arc but browser callbacks remain confined to one Worker"
    )]
    pub fn shared(
        request: js_sys::Function,
        cancel: js_sys::Function,
    ) -> Arc<dyn TableStructureEngine> {
        Arc::new(Self { request, cancel })
    }
}

/// A pending provider promise is canceled if Rust times out or the parse task is dropped.
struct PendingRequest {
    id: String,
    cancel: js_sys::Function,
    settled: bool,
}
impl Drop for PendingRequest {
    /// Releases the JS broker entry and notifies the calling thread about cancellation.
    fn drop(&mut self) {
        if !self.settled {
            let _ = self
                .cancel
                .call1(&JsValue::NULL, &JsValue::from_str(&self.id));
        }
    }
}

impl TableStructureEngine for BrowserTableEngine {
    /// Identifies the external callback boundary without claiming a particular model.
    fn name(&self) -> &str {
        "browser-external-tsr"
    }

    /// Copies pixel data before invoking JavaScript and waits only on the correlated promise.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<TsrTableInput, TableStructureError>,
    > {
        Box::pin(async move {
            let metadata = CropMetadata::builder()
                .request_id(&request.request_id)
                .page_number(request.page_number)
                .block_id(&request.block_id)
                .crop_bbox(request.crop_bbox)
                .crop_to_viewport(request.crop_to_viewport)
                .reason(&request.reason)
                .width(request.image.width())
                .height(request.image.height())
                .build();
            let wire = metadata
                .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
                .map_err(|e| TableStructureError::Engine {
                    message: e.to_string(),
                })?;
            // Uint8Array::from owns its storage; a borrowed WASM view cannot survive the external await.
            let pixels =
                js_sys::Uint8Array::from(request.image.data().as_ref());
            js_sys::Reflect::set(&wire, &JsValue::from_str("pixels"), &pixels)
                .map_err(|error| TableStructureError::Engine {
                    message: js_message(error),
                })?;
            let mut pending = PendingRequest {
                id: request.request_id.clone(),
                cancel: self.cancel.clone(),
                settled: false,
            };
            let value =
                self.request.call1(&JsValue::NULL, &wire).map_err(|error| {
                    TableStructureError::Engine {
                        message: js_message(error),
                    }
                })?;
            let value = wasm_bindgen_futures::JsFuture::from(
                js_sys::Promise::resolve(&value),
            )
            .await
            .map_err(|error| TableStructureError::Engine {
                message: js_message(error),
            })?;
            pending.settled = true;
            serde_wasm_bindgen::from_value(value).map_err(|e| {
                TableStructureError::InvalidInput {
                    reason: e.to_string(),
                }
            })
        })
    }
}

/// Validated access to the per-call Worker callback object.
pub(super) struct Callbacks(JsValue);
impl From<JsValue> for Callbacks {
    /// Missing callbacks are represented by an empty object, preserving legacy calls.
    fn from(value: JsValue) -> Self {
        Self(if value.is_null() || value.is_undefined() {
            js_sys::Object::new().into()
        } else {
            value
        })
    }
}
impl Callbacks {
    /// Checks each callback before exposing it to an async Rust extension point.
    pub fn function(
        &self,
        name: &str,
    ) -> Result<Option<js_sys::Function>, JsValue> {
        let value = js_sys::Reflect::get(&self.0, &JsValue::from_str(name))
            .map_err(|_error| {
                super::WebError::value(
                    "InvalidOptions",
                    format!("cannot read callback {name}"),
                )
            })?;
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }
        value.dyn_into().map(Some).map_err(|_error| {
            super::WebError::value(
                "InvalidOptions",
                format!("callback {name} must be a function"),
            )
        })
    }
}

/// Converts arbitrary JS failures to owned messages without retaining JS handles across core boundaries.
fn js_message(value: JsValue) -> String {
    js_sys::Reflect::get(&value, &JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| "external table callback rejected".to_owned())
}
