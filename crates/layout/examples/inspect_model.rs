use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use docparse_layout::inspect_model;

/// Paths accepted by the model schema inspection utility.
#[derive(Debug, Parser)]
struct Arguments {
    /// Fixed ONNX model to inspect.
    model_path: PathBuf,
    /// JSON fixture destination.
    output_path: PathBuf,
}

/// Inspects one ONNX model and writes deterministic neutral JSON schema.
fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let schema = inspect_model(&arguments.model_path)?;
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
