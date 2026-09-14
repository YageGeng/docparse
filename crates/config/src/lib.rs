//! Instance-scoped configuration loading and validation for docparse.

mod config;
mod error;
mod validate;
mod wasm_compat;

pub use config::{
    DatabaseConfig, FusionConfig, LayoutConfig, LogConfig, ModelFiles,
    OcrConfig, OcrPolicy, OutputConfig, RawConfig, RenderConfig, RuntimeConfig,
    ServerConfig, TableCellConfig, TableCellModel, TableMode, TsrConfig,
    TsrModel,
};
pub use error::ConfigError;

pub use validate::ValidatedConfig;
#[allow(unused_imports)]
pub use wasm_compat::*;
