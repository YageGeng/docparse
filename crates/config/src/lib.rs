//! Instance-scoped configuration loading and validation for docparse.

mod error;
mod types;
mod validate;
mod wasm_compat;

pub use error::ConfigError;
pub use types::{
    ExecutionProviderConfig, FusionConfig, LayoutConfig, OcrConfig, OcrPolicy,
    OutputConfig, RawConfig, RenderConfig, RuntimeConfig,
};

pub use validate::ValidatedConfig;
#[allow(unused_imports)]
pub use wasm_compat::*;
