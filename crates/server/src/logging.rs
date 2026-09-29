//! Log destinations and event filtering preserve correlation without terminal escapes in files.
use docparse_config::LogConfig;
use std::{fs::OpenOptions, io::IsTerminal};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::field::MakeExt;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::layer::SubscriberExt;

/// Builds terminal output plus optional append-only file logging with a shared environment filter.
pub fn subscriber(
    config: &LogConfig,
) -> Result<
    (
        impl tracing::Subscriber + Send + Sync,
        Vec<tracing_appender::non_blocking::WorkerGuard>,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let environment = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.directives))?;
    // Bounded, lossy queues keep slow disks/pipes off request threads. Guards flush on shutdown.
    let (stdout, stdout_guard) =
        tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(8192)
            .lossy(true)
            .thread_name("docparse-log-stdout")
            .finish(std::io::stdout());
    let mut guards = vec![stdout_guard];
    let file_layer = if let Some(path) = &config.file {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        // Append preserves earlier profiling evidence across service restarts.
        let (writer, guard) =
            tracing_appender::non_blocking::NonBlockingBuilder::default()
                .buffered_lines_limit(8192)
                .lossy(true)
                .thread_name("docparse-log-file")
                .finish(
                    OpenOptions::new().create(true).append(true).open(path)?,
                );
        guards.push(guard);
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                // A distinct formatter type isolates span caches from stdout without changing field text.
                .fmt_fields(DefaultFields::new().delimited(""))
                .with_writer(writer),
        )
    } else {
        None
    };
    Ok((
        tracing_subscriber::registry()
            .with(filter(environment))
            // Keep stdout active when file logging is configured, with color only on terminals.
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(std::io::stdout().is_terminal())
                    .with_writer(stdout),
            )
            .with(file_layer),
        guards,
    ))
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
