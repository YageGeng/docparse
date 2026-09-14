//! Warm-model native parsing benchmark using the default local PDFium provider.
#[path = "benchmark/common.rs"]
mod common;

/// Runs shared measurements without requiring a worker executable.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    common::run(
        std::sync::Arc::new(docparse_core::LocalPdfiumProvider),
        None,
    )
    .await
}
