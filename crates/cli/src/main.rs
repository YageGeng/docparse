use clap::Parser;
use docparse_cli::args::Cli;

/// Initializes stderr diagnostics and executes the selected asynchronous command.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Some(output) = docparse_cli::run(Cli::parse()).await? {
        println!("{output}");
    }
    Ok(())
}
