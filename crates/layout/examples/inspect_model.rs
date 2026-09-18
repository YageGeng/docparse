use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_layout::{OnnxBackend, inspect_model};

/// Paths accepted by the model schema inspection utility.
#[derive(Debug, Parser)]
struct Arguments {
    /// Fixed ONNX model to inspect.
    model_path: PathBuf,
    /// JSON fixture destination.
    output_path: PathBuf,
    /// Parser configuration supplying the shared ONNX optimization policy.
    #[arg(long, default_value = "docparse.toml")]
    config: PathBuf,
}

/// Inspects one ONNX model and writes deterministic neutral JSON schema.
fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    // Use the same configuration source as normal parsing instead of ORT's implicit defaults.
    let config = ValidatedConfig::try_from(
        ConfigLoader::new(arguments.config).load_raw()?,
    )?;
    let schema =
        inspect_model(&arguments.model_path, OnnxBackend::from(&config))?;
    let payload = serde_json::to_string_pretty(&schema)
        .context("failed to serialize model schema")?
        + "\n";
    if let Some(parent) = arguments.output_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create output directory {}", parent.display())
        })?;
    }
    fs::write(&arguments.output_path, payload).with_context(|| {
        format!("failed to write {}", arguments.output_path.display())
    })?;
    Ok(())
}
