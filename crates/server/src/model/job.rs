use docparse_database::JobStatus;
use docparse_database::entities::parse_jobs::Model;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;
use uuid::Uuid;

/// Identifies a durable job through query parameters, including native EventSource subscriptions.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct JobQuery {
    /// UUID returned on submission, equal to the upload's Idempotency-Key.
    pub id: Uuid,
}

/// Multipart contract for a streamed upload; the handler continues to process chunks instead of buffering this DTO.
#[derive(utoipa::ToSchema)]
pub struct PdfUpload {
    /// The sole PDF file, beginning with the %PDF- signature.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

/// Public progress snapshots omit storage names, lease tokens, and other worker-only fields.
#[derive(Debug, Serialize, TypedBuilder, utoipa::ToSchema)]
pub struct JobSnapshot {
    pub id: Uuid,
    /// The shared task enum retains the existing lowercase wire values.
    pub status: JobStatus,
    pub version: i64,
    pub attempts: i32,
    #[builder(default)]
    #[schema(value_type = Option<docparse_core::ParseProgress>)]
    pub progress: Option<serde_json::Value>,
    #[builder(default)]
    pub error: Option<String>,
}

impl From<Model> for JobSnapshot {
    /// Copies only the public task contract from a generated database entity.
    fn from(job: Model) -> Self {
        Self::builder()
            .id(job.id)
            .status(job.status)
            .version(job.version)
            .attempts(job.attempts)
            .progress(job.progress)
            .error(job.error)
            .build()
    }
}

impl JobSnapshot {
    /// Indicates when clients can stop reconnecting to the SSE subscription.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}
