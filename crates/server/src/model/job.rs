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

/// Selects a stored document representation without rerunning PDF or model inference.
#[derive(Debug, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultFormat {
    /// Canonical JSON, including both formula LaTeX and Markdown.
    #[default]
    Json,
    /// Complete document Markdown with inline, display and table formulas.
    Markdown,
}

/// A durable task identifier and an optional output representation.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct JobResultQuery {
    /// UUID returned by the upload endpoint.
    pub id: Uuid,
    /// Defaults to json; markdown returns text/markdown rather than a JSON envelope.
    #[serde(default)]
    #[param(inline)]
    pub format: Option<ResultFormat>,
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
    #[builder(default)]
    pub filename: Option<String>,
    #[builder(default)]
    pub size_bytes: Option<i64>,
    #[schema(format = DateTime)]
    pub created_at: String,
    #[schema(format = DateTime)]
    pub updated_at: String,
    /// The shared task enum retains the existing lowercase wire values.
    pub status: JobStatus,
    pub version: i64,
    pub attempts: i32,
    /// Latest completed attempt's elapsed milliseconds, including parsing and result publication; excludes upload, queueing, and the final database update.
    #[builder(default)]
    #[schema(minimum = 0)]
    pub duration_ms: Option<i64>,
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
            // Presentation metadata remains separate from immutable storage names and worker fencing fields.
            .filename(job.filename)
            .size_bytes(job.size_bytes)
            .created_at(job.created_at.to_rfc3339())
            .updated_at(job.updated_at.to_rfc3339())
            .status(job.status)
            .version(job.version)
            .attempts(job.attempts)
            .duration_ms(job.duration_ms)
            .progress(job.progress)
            .error(job.error)
            .build()
    }
}

/// Bounds history queries and retains the caller's filter when navigating cursor pages.
#[derive(Debug, Deserialize, utoipa::IntoParams, TypedBuilder)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct JobListQuery {
    #[builder(default)]
    pub cursor: Option<Uuid>,
    #[builder(default)]
    pub limit: Option<u64>,
    #[builder(default)]
    pub status: Option<JobStatus>,
    #[builder(default)]
    pub search: Option<String>,
}

/// A history page includes a cursor only when another matching row exists.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct JobList {
    pub items: Vec<JobSnapshot>,
    pub next_cursor: Option<Uuid>,
}

impl JobSnapshot {
    /// Indicates when clients can stop reconnecting to the SSE subscription.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}
