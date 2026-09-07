use crate::{LayoutDetection, LayoutError, LayoutRequest};

/// Asynchronous, runtime-selectable page layout detection boundary.
pub trait LayoutEngine:
    crate::wasm_compat::WasmCompatSend + crate::wasm_compat::WasmCompatSync
{
    /// Returns a stable diagnostic engine name.
    fn name(&self) -> &str;

    /// Returns the immutable model revision used by this engine.
    fn model_revision(&self) -> &str;

    /// Detects layout regions for one rendered page image.
    fn detect(
        &self,
        request: LayoutRequest,
    ) -> crate::wasm_compat::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    >;
}
