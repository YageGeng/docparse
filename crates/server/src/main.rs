use clap::{Parser, ValueEnum};
use docparse_config::{
    ConfigError, ConfigLoader, RawConfig, ServerConfig, ValidatedConfig,
};
use docparse_core::DocParser;
use docparse_database::connection;
use docparse_server::{
    app,
    cleanup::DeletedResults,
    logging,
    state::{AppState, HttpOptions},
    storage::SharedStorage,
    worker::{Worker, WorkerOptions},
};
use std::{
    error::Error, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration,
};
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

/// The same deployment artifact can expose HTTP, consume GPU jobs, or do both.
#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Api,
    Worker,
    All,
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
    #[arg(long, default_value_t = 2)]
    worker_concurrency: usize,
    #[arg(long, default_value_t = 536870912)]
    max_upload_bytes: usize,
    #[arg(long, default_value_t = 4)]
    max_uploads: usize,
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

    // Load file/profile overrides first; a valid RUST_LOG retains its precedence over configured directives.
    let directives = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| {
            tracing_subscriber::EnvFilter::try_new(&raw.log.directives)
        })?;
    tracing_subscriber::fmt()
        .with_env_filter(logging::filter(directives))
        .init();

    let server = raw.server.clone();
    let state = AppState::new(
        connection::connect(&raw.database).await?,
        SharedStorage::new(&arguments.storage_dir).await?,
        HttpOptions::from(&arguments),
        CancellationToken::new(),
    )?;
    let worker = arguments.build_worker(raw, &state).await?;
    run_services(arguments.role, server, state, worker).await
}

impl Arguments {
    /// Applies legacy CLI/environment overrides after the shared configuration layers, then validates the listener.
    fn load_config(&self) -> Result<RawConfig, ConfigError> {
        let mut raw = ConfigLoader::new(&self.config).load_raw()?;
        if let Some(bind) = self.bind {
            raw.server.host = bind.ip().to_string();
            raw.server.port = bind.port();
        }
        if let Some(url) = &self.database_url {
            raw.database.url = url.clone();
        }
        raw.server.validate()?;
        Ok(raw)
    }

    /// Creates one reusable parser and worker only for roles that consume jobs.
    async fn build_worker(
        &self,
        raw: RawConfig,
        state: &AppState,
    ) -> Result<Option<Worker>, Box<dyn Error>> {
        // Keep the API-only path independent of parser validation and model initialization.
        if matches!(self.role, Role::Api) {
            return Ok(None);
        }
        let config = Arc::new(ValidatedConfig::try_from(raw)?);
        let parser = Arc::new(
            DocParser::builder()
                .config(Arc::clone(&config))
                .build()
                .await?,
        );
        Ok(Some(
            Worker::builder()
                .db(state.db.clone())
                .storage(state.storage.clone())
                .parser(parser)
                .output(config.output().clone())
                .options(WorkerOptions::from(self))
                .build(),
        ))
    }
}

impl From<&Arguments> for HttpOptions {
    /// Converts CLI upload limits into the existing HTTP options.
    fn from(arguments: &Arguments) -> Self {
        Self::builder()
            .max_upload_bytes(arguments.max_upload_bytes)
            .max_uploads(arguments.max_uploads)
            .upload_timeout(Duration::from_secs(
                arguments.upload_timeout_seconds,
            ))
            .build()
    }
}

impl From<&Arguments> for WorkerOptions {
    /// Maps process limits to the worker's existing lease and timeout policy.
    fn from(arguments: &Arguments) -> Self {
        Self::builder()
            .concurrency(arguments.worker_concurrency)
            .lease_seconds(arguments.lease_seconds)
            .max_attempts(arguments.max_attempts)
            .job_timeout(Duration::from_secs(arguments.job_timeout_seconds))
            .build()
    }
}

/// Owns signal registration, concurrent API/worker execution, and cleanup for every process role.
async fn run_services(
    role: Role,
    server: ServerConfig,
    state: AppState,
    worker: Option<Worker>,
) -> Result<(), Box<dyn Error>> {
    let shutdown = state.shutdown.clone();
    let cleanup = DeletedResults::from(&state);
    // Register SIGTERM before accepting traffic and keep both services on the same cancellation token.
    let mut terminate = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    )?;
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
        tracing::info!("HTTP server listening on {}", listener.local_addr()?);
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
    let outcome = tokio::try_join!(serving, working, cleaning);
    // Always release the signal task after either normal draining or a service error.
    shutdown.cancel();
    signal.abort();
    outcome.map(|_| ())
}
