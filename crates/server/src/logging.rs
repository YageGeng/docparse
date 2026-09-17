//! Log destinations and event filtering preserve correlation without terminal escapes in files.
use docparse_config::LogConfig;
use std::{fs::OpenOptions, io::IsTerminal};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

/// Builds append-only file logging or terminal output while retaining environment-filter precedence.
pub fn subscriber(
    config: &LogConfig,
) -> Result<
    impl tracing::Subscriber + Send + Sync,
    Box<dyn std::error::Error + Send + Sync>,
> {
    let environment = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.directives))?;
    let writer = if let Some(path) = &config.file {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        // Append preserves earlier profiling evidence across service restarts.
        BoxMakeWriter::new(
            OpenOptions::new().create(true).append(true).open(path)?,
        )
    } else {
        BoxMakeWriter::new(std::io::stdout)
    };
    Ok(tracing_subscriber::fmt()
        // ANSI sequences belong only in interactive terminals, never files or redirected stdout.
        .with_ansi(config.file.is_none() && std::io::stdout().is_terminal())
        .with_writer(writer)
        .with_env_filter(filter(environment))
        .finish())
}

/// Reserved for correlation spans; ordinary events retain their module targets and RUST_LOG levels.
pub(crate) const CONTEXT_TARGET: &str = "docparse::context";

/// Keeps correlation metadata while applying the caller's filter to ordinary log events.
pub fn filter(environment: EnvFilter) -> EnvFilter {
    // A target directive enables only these spans; a span-name directive would also enable their child events.
    environment.add_directive(
        format!("{CONTEXT_TARGET}=info").parse().expect(
            "the static correlation target is a valid tracing directive",
        ),
    )
}
