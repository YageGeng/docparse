//! Instance-scoped configuration loading and validation for docparse.

mod config;
mod error;
mod validate;
mod wasm_compat;

pub use config::{
    DatabaseConfig, FormulaBackpressureConfig, FormulaConfig,
    FormulaEngineConfig, FusionConfig, HttpFormulaConfig, LayoutConfig,
    LogConfig, ModelFiles, OcrConfig, OcrModelConfig, OcrPolicy,
    OptimizationLevel, OutputConfig, PpFormulaConfig, RawConfig, RenderConfig,
    RuntimeConfig, ServerConfig, TableCellConfig, TableCellModel, TableMode,
    TexoFormulaConfig, TsrConfig, TsrModel,
};
pub use error::ConfigError;

pub use validate::ValidatedConfig;
#[allow(unused_imports)]
pub use wasm_compat::*;
