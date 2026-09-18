use crate::{
    code::ApiCode,
    error::{ApiResult, RequestSnafu},
    storage::SharedStorage,
};
use docparse_database::seaorm::DatabaseConnection;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

/// HTTP limits apply before input buffering; SSE polling is independent of worker progress delivery.
#[derive(Clone, TypedBuilder)]
pub struct HttpOptions {
    /// Markdown projections use the same placeholder policy as the configured worker.
    #[builder(default)]
    pub output: docparse_config::OutputConfig,
    #[builder(default = 512 * 1024 * 1024)]
    pub max_upload_bytes: usize,
    #[builder(default = 4)]
    pub max_uploads: usize,
    #[builder(default = Duration::from_secs(300))]
    pub upload_timeout: Duration,
    #[builder(default = Duration::from_secs(1))]
    pub poll_interval: Duration,
}

/// API instances hold only shared handles, never an in-memory registry of durable jobs.
#[derive(Clone, TypedBuilder)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub storage: SharedStorage,
    pub options: HttpOptions,
    pub uploads: Arc<Semaphore>,
    pub shutdown: CancellationToken,
    /// Shares database polling per job without imposing a subscriber or request limit.
    #[builder(default)]
    pub(crate) subscriptions: Arc<crate::routers::jobs::Subscriptions>,
}

impl AppState {
    /// Validates memory and timing limits before constructing the router's shared state.
    pub fn new(
        db: DatabaseConnection,
        storage: SharedStorage,
        options: HttpOptions,
        shutdown: CancellationToken,
    ) -> ApiResult<Self> {
        if options.max_upload_bytes == 0
            || options.max_upload_bytes > i32::MAX as usize
            || options.max_uploads == 0
            || options.max_uploads > 1024
            || options.upload_timeout.is_zero()
            || options.poll_interval.is_zero()
        {
            // Configuration failures now retain their location through ErrorCode::message.
            return RequestSnafu {
                stage: "http-validate-options",
                code: ApiCode::COMMON_BAD_REQUEST,
            }
            .fail();
        }
        Ok(Self::builder()
            .db(db)
            .storage(storage)
            .uploads(Arc::new(Semaphore::new(options.max_uploads)))
            .options(options)
            .shutdown(shutdown)
            .build())
    }
}
