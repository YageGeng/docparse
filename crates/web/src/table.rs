//! Worker-local bridge to caller-owned external table structure callbacks.
use std::sync::Arc;

use crate::js::{self, FunctionExt, ValueExt};
use docparse_core::{
    BlockId, TableStructureEngine, TableStructureError, TsrRequestReason,
    TsrTableInput, TsrTableRequest,
};
use docparse_layout::{AffineTransform, Bbox};
use serde::Serialize;
use typed_builder::TypedBuilder;
use wasm_bindgen::prelude::*;

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
    request: js::Function,
    cancel: js::Function,
}

impl BrowserTableEngine {
    /// Owns both halves of the broker so dropping a pending Rust future also cancels its JS request.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "the core API uses Arc but browser callbacks remain confined to one Worker"
    )]
    pub fn shared(
        request: js::Function,
        cancel: js::Function,
    ) -> Arc<dyn TableStructureEngine> {
        Arc::new(Self { request, cancel })
    }
}

/// A pending provider promise is canceled if Rust times out or the parse task is dropped.
struct PendingRequest {
    id: String,
    cancel: js::Function,
    settled: bool,
}
impl Drop for PendingRequest {
    /// Releases the JS broker entry and notifies the calling thread about cancellation.
    fn drop(&mut self) {
        if !self.settled
            && let Err(error) =
                self.cancel.invoke(&[JsValue::from_str(&self.id)])
        {
            tracing::warn!(
                "table request {} cancellation callback failed: {}",
                self.id,
                error.exception_message()
            );
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
            let pixels = js::copy_pixels(request.image.data().as_ref());
            wire.set_property("pixels", &pixels).map_err(|error| {
                TableStructureError::Engine {
                    message: error.exception_message(),
                }
            })?;
            let mut pending = PendingRequest {
                id: request.request_id.clone(),
                cancel: self.cancel.clone(),
                settled: false,
            };
            let value =
                self.request.invoke_async(&wire).await.map_err(|error| {
                    TableStructureError::Engine {
                        message: error.exception_message(),
                    }
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
