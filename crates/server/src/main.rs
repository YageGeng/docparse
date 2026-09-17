use clap::{Parser, ValueEnum};
use docparse_config::{
    ConfigError, ConfigLoader, RawConfig, ServerConfig, ValidatedConfig,
};
use docparse_core::{DocParser, PdfiumProvider};
use docparse_database::connection;
use docparse_server::{
    app,
    cleanup::DeletedResults,
    logging,
    pdfium_pool::PdfiumPool,
    state::{AppState, HttpOptions},
    storage::SharedStorage,
    worker::{Worker, WorkerOptions},
};
use std::{
    error::Error, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration,
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::util::SubscriberInitExt;
use typed_builder::TypedBuilder;

/// The same deployment artifact can expose HTTP, consume GPU jobs, or do both.
#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Api,
    Worker,
    All,
}

/// Keeps the shared process pool alive through durable-worker draining and explicit shutdown.
struct ActiveWorker {
    worker: Worker,
    pool: Arc<PdfiumPool>,
}

/// Process controls and explicit overrides complement the shared server and database configuration.
#[derive(Parser, TypedBuilder)]
#[command(about = "Durable PDF jobs with PostgreSQL, shared storage, and SSE")]
struct Arguments {
    #[arg(long, env = "SERVER_ROLE", value_enum, default_value = "all")]
    role: Role,
    #[builder(default)]
    #[arg(long, env = "SERVER_BIND")]
    bind: Option<SocketAddr>,
    #[builder(default)]
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,
    #[arg(long, env = "SERVER_STORAGE_DIR", default_value = "./data/docparse")]
    storage_dir: PathBuf,
    #[arg(long, default_value = "docparse.toml")]
    config: PathBuf,
    /// Overrides server.worker_concurrency from the shared configuration.
    #[builder(default)]
    #[arg(long)]
    worker_concurrency: Option<usize>,
    #[arg(long, default_value_t = 536870912)]
    max_upload_bytes: usize,
    /// Overrides server.max_uploads from the shared configuration.
    #[builder(default)]
    #[arg(long)]
    max_uploads: Option<usize>,
    #[arg(long, default_value_t = 300)]
    upload_timeout_seconds: u64,
    #[arg(long, default_value_t = 60)]
    lease_seconds: i32,
    #[arg(long, default_value_t = 3)]
    max_attempts: i32,
    #[arg(long, default_value_t = 3600)]
    job_timeout_seconds: u64,
}

/// Starts the configured API and worker after initializing their shared resources.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse();
    let raw = arguments.load_config()?;

    // Open the configured destination before connections/models, and retain the standard log bridge.
    logging::subscriber(&raw.log)
        .map_err(|error| -> Box<dyn Error> { error })?
        .try_init()?;

    let server = raw.server.clone();
    let mut http_options = arguments.http_options(&server);
    http_options.output = raw.output.clone();
    let state = AppState::new(
        connection::connect(&raw.database).await?,
        SharedStorage::new(&arguments.storage_dir).await?,
        http_options,
        CancellationToken::new(),
    )?;
    let worker = arguments.build_worker(raw, &state).await?;
    run_services(arguments.role, server, state, worker).await
}

impl Arguments {
    /// Applies explicit CLI/environment overrides before validating listener, upload, and worker limits.
    fn load_config(&self) -> Result<RawConfig, ConfigError> {
        let mut raw = ConfigLoader::new(&self.config).load_raw()?;
        if let Some(bind) = self.bind {
            raw.server.host = bind.ip().to_string();
            raw.server.port = bind.port();
        }
        if let Some(url) = &self.database_url {
            raw.database.url = url.clone();
        }
        if let Some(concurrency) = self.worker_concurrency {
            raw.server.worker_concurrency = concurrency;
        }
        if let Some(max_uploads) = self.max_uploads {
            raw.server.max_uploads = max_uploads;
        }
        raw.server.validate()?;
        Ok(raw)
    }

    /// Uses resolved upload admission alongside the existing CLI size and timeout limits.
    fn http_options(&self, server: &ServerConfig) -> HttpOptions {
        HttpOptions::builder()
            .max_upload_bytes(self.max_upload_bytes)
            .max_uploads(server.max_uploads)
            .upload_timeout(Duration::from_secs(self.upload_timeout_seconds))
            .build()
    }

    /// Uses resolved document concurrency alongside the existing CLI lease and timeout policy.
    fn worker_options(&self, server: &ServerConfig) -> WorkerOptions {
        WorkerOptions::builder()
            .concurrency(server.worker_concurrency)
            .lease_seconds(self.lease_seconds)
            .max_attempts(self.max_attempts)
            .job_timeout(Duration::from_secs(self.job_timeout_seconds))
            .build()
    }

    /// Creates one reusable parser and worker only for roles that consume jobs.
    async fn build_worker(
        &self,
        raw: RawConfig,
        state: &AppState,
    ) -> Result<Option<ActiveWorker>, Box<dyn Error>> {
        // Keep the API-only path independent of parser validation and model initialization.
        if matches!(self.role, Role::Api) {
            return Ok(None);
        }
        let max_processes = raw.server.pdfium_max_workers;
        let options = self.worker_options(&raw.server);
        tracing::info!(
            "starting parser with document concurrency {} and at most {} PDFium workers",
            options.concurrency,
            max_processes
        );
        let config = Arc::new(ValidatedConfig::try_from(raw)?);
        let pool =
            PdfiumPool::start(max_processes, &std::env::current_exe()?).await?;
        let parser = match DocParser::builder()
            .config(Arc::clone(&config))
            .pdfium_provider(Arc::clone(&pool) as Arc<dyn PdfiumProvider>)
            .build()
            .await
        {
            Ok(parser) => Arc::new(parser),
            Err(error) => {
                tracing::error!(
                    "parser initialization failed; stopping PDFium workers: {}",
                    error
                );
                if let Err(cleanup) = pool.shutdown().await {
                    tracing::error!(
                        "PDFium cleanup after parser initialization failed: {}",
                        cleanup
                    );
                }
                return Err(error.into());
            }
        };
        Ok(Some(ActiveWorker {
            worker: Worker::builder()
                .db(state.db.clone())
                .storage(state.storage.clone())
                .parser(parser)
                .output(config.output().clone())
                .options(options)
                .build(),
            pool,
        }))
    }
}

/// Owns signal registration, concurrent API/worker execution, and cleanup for every process role.
async fn run_services(
    role: Role,
    server: ServerConfig,
    state: AppState,
    worker: Option<ActiveWorker>,
) -> Result<(), Box<dyn Error>> {
    let (worker, pool): (Option<Worker>, Option<Arc<PdfiumPool>>) =
        worker.map(|active| (active.worker, active.pool)).unzip();
    let shutdown = state.shutdown.clone();
    let cleanup = DeletedResults::from(&state);
    // Register SIGTERM before accepting traffic and keep both services on the same cancellation token.
    let mut terminate = match tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    ) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::error!(
                "failed to register server shutdown signal: {}",
                error
            );
            if let Some(pool) = &pool
                && let Err(cleanup) = pool.shutdown().await
            {
                tracing::error!(
                    "PDFium cleanup after signal setup failed: {}",
                    cleanup
                );
            }
            return Err(error.into());
        }
    };
    let stopping = shutdown.clone();
    let signal = tokio::spawn(async move {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        stopping.cancel();
    });
    let serving = async {
        if matches!(role, Role::Worker) {
            return Ok::<(), Box<dyn Error>>(());
        }
        let listener =
            tokio::net::TcpListener::bind((server.host.as_str(), server.port))
                .await?;
        tracing::info!(
            "HTTP server listening on {} with at most {} concurrent uploads",
            listener.local_addr()?,
            state.options.max_uploads
        );
        // The same configuration drives binding and the final documented route prefix.
        axum::serve(listener, app::router(state, &server)?)
            .with_graceful_shutdown(shutdown.clone().cancelled_owned())
            .await?;
        Ok(())
    };
    let working = async {
        if let Some(worker) = worker {
            worker.run(shutdown.clone()).await?;
        }
        Ok::<(), Box<dyn Error>>(())
    };
    // Cleanup recovery also runs for API-only deployments and never competes for parser concurrency slots.
    let cleaning = async {
        cleanup.run(shutdown.clone()).await;
        Ok::<(), Box<dyn Error>>(())
    };
    let monitoring = async {
        if let Some(pool) = &pool {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {}
                _ = pool.stopped() => return Err::<(), Box<dyn Error>>(
                    std::io::Error::other("PDFium pool became unavailable").into()),
            }
        } else {
            shutdown.cancelled().await;
        }
        Ok::<(), Box<dyn Error>>(())
    };
    let outcome = tokio::try_join!(serving, working, cleaning, monitoring);
    // Always release the signal task after either normal draining or a service error.
    shutdown.cancel();
    signal.abort();
    if let Some(pool) = pool
        && let Err(error) = pool.shutdown().await
    {
        tracing::error!("PDFium pool shutdown failed: {}", error);
        if outcome.is_ok() {
            return Err(error.into());
        }
    }
    outcome.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Omitted CLI flags retain configuration, while explicit flags override it before validation.
    #[test]
    fn concurrency_uses_configuration_unless_cli_overrides_it() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let path = directory.path().join("docparse.toml");
        for (field, flag, default) in [
            ("worker_concurrency", "--worker-concurrency", 2),
            ("max_uploads", "--max-uploads", 4),
        ] {
            for (configured, override_value, expected) in [
                (None, None, default),
                (Some(8), None, 8),
                (Some(8), Some(3), 3),
                (Some(0), Some(3), 3),
            ] {
                let content = configured
                    .map(|value| format!("[server]\n{field} = {value}\n"))
                    .unwrap_or_default();
                std::fs::write(&path, content).expect("configuration file");
                let mut argv = vec![
                    std::ffi::OsString::from("docparse-server"),
                    "--config".into(),
                    path.as_os_str().to_owned(),
                ];
                if let Some(value) = override_value {
                    argv.extend([flag.into(), value.to_string().into()]);
                }
                let arguments =
                    Arguments::try_parse_from(argv).expect("CLI arguments");
                let raw =
                    arguments.load_config().expect("effective configuration");
                let server = serde_json::to_value(&raw.server)
                    .expect("server configuration");
                assert_eq!(
                    server.get(field),
                    Some(&serde_json::json!(expected))
                );
                let applied = if field == "max_uploads" {
                    arguments.http_options(&raw.server).max_uploads
                } else {
                    arguments.worker_options(&raw.server).concurrency
                };
                assert_eq!(applied, expected);
            }
        }
    }

    /// Invalid explicit overrides must fail before database connections or model initialization.
    #[test]
    fn concurrency_rejects_invalid_cli_overrides() {
        let directory = tempfile::tempdir().expect("configuration directory");
        let path = directory.path().join("docparse.toml");
        std::fs::write(&path, "").expect("configuration file");
        for (field, flag, limits) in [
            ("worker_concurrency", "--worker-concurrency", [0, 129]),
            ("max_uploads", "--max-uploads", [0, 1025]),
        ] {
            for limit in limits {
                let arguments = Arguments::try_parse_from([
                    std::ffi::OsString::from("docparse-server"),
                    "--config".into(),
                    path.as_os_str().to_owned(),
                    flag.into(),
                    limit.to_string().into(),
                ])
                .expect("numeric CLI value");
                assert!(
                    matches!(arguments.load_config(), Err(ConfigError::InvalidValue { field: actual, .. })
                    if actual == format!("server.{field}")),
                    "invalid CLI concurrency must be rejected during configuration loading"
                );
            }
        }
    }
}
