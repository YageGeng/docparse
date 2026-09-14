//! Exercises the production server pool with the same measurements as the local benchmark.
#[path = "../../core/examples/benchmark/common.rs"]
mod common;

use docparse_core::PdfiumProvider;
use docparse_server::pdfium_pool::PdfiumPool;
use std::{path::PathBuf, sync::Arc};

/// Starts the configured worker pool, runs measurements, and explicitly reaps every child on failure.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config =
        PathBuf::from(std::env::args_os().nth(1).ok_or("missing config path")?);
    let raw = docparse_config::ConfigLoader::new(config).load_raw()?;
    raw.server.validate()?;
    let count = std::env::args_os()
        .nth(6)
        .map(|s| s.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(raw.server.pdfium_max_workers);
    let pool = PdfiumPool::start(count, &std::env::current_exe()?).await?;
    let result =
        common::run(Arc::clone(&pool) as Arc<dyn PdfiumProvider>, Some(count))
            .await;
    let cleanup = pool.shutdown().await;
    result?;
    cleanup?;
    Ok(())
}
