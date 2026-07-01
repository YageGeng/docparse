//! PP-DocLayoutV3 native Burn runtime.

mod config;
mod model;
mod ops;
mod postprocess;
mod preprocess;
mod weights;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use model::load_pp_doclayout_runtime;
pub(crate) use model::{
    PpDocLayoutRuntime, load_pp_doclayout_runtime_from_bytes,
};
pub(crate) use postprocess::LayoutDetection;
