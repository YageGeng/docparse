/// Runs the isolated PDFium worker without initializing inference models.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    if let Err(error) = docparse_core::pdfium_ipc::run_worker() {
        tracing::error!("PDFium worker stopped with an error: {}", error);
        return Err(error.into());
    }
    Ok(())
}
