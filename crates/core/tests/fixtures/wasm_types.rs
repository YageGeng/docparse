use docparse_layout::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest, WasmBoxedFuture,
};
use std::rc::Rc;

/// A real non-Send value exercises the browser-only relaxation of the engine contract.
pub struct LocalEngine(pub Rc<()>);

impl LayoutEngine for LocalEngine {
    /// Identifies this compile-only boundary sample.
    fn name(&self) -> &str {
        "local-type-check"
    }
    /// Supplies a stable identity for the compile-only sample.
    fn model_revision(&self) -> &str {
        "compile-only"
    }
    /// Retains an Rc borrow across an await to require a genuinely local future.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async move {
            let retained = Rc::clone(&self.0);
            std::future::ready(()).await;
            drop(retained);
            Ok(Vec::new())
        })
    }
}
