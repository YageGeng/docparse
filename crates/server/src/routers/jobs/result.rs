use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu, StorageSnafu},
    model::{base::ApiResponse, error::ApiErrorResponse, job::JobQuery},
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Query, State, rejection::QueryRejection},
    http::header,
    response::{IntoResponse, Response},
};
use docparse_core::DocumentResult;
use docparse_database::{JobStatus, query::parse_job::ParseJobQuery as Jobs};
use snafu::{OptionExt, ResultExt};
use tokio_util::io::ReaderStream;

/// Streams the immutable successful JSON envelope from shared storage rather than loading it into API memory.
#[utoipa::path(
    get, path = "/jobs/result", tag = super::TAG,
    description = "Stream the persisted ApiResponse<DocumentResult> JSON after success. Result visibility follows the worker output configuration. Model/page warnings remain in the canonical result. This endpoint does not rerun parsing.",
    params(JobQuery),
    responses(
        (status = 200, description = "Configured canonical document wrapped in the common success envelope", body = ApiResponse<DocumentResult>),
        (status = 400, description = "Invalid task UUID (4001002)", body = ApiErrorResponse),
        (status = 404, description = "Task not found (4041001)", body = ApiErrorResponse),
        (status = 409, description = "Result not ready (4091002) or task failed (4091003)", body = ApiErrorResponse),
        (status = 503, description = "Database or shared storage unavailable (5031002-5031003)", body = ApiErrorResponse),
        (status = 500, description = "Internal result state error (500000)", body = ApiErrorResponse)
    )
)]
pub async fn result(
    State(state): State<AppState>,
    query: Result<Query<JobQuery>, QueryRejection>,
) -> ApiResult<Response> {
    // Query extraction keeps task identifiers out of route templates and works with native EventSource.
    let Query(JobQuery { id }) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "job-result-parse-id",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    tracing::Span::current().record("job_id", tracing::field::display(id));
    let job = Jobs::find_by_id(&state.db, id)
        .await
        .with_context(|source| DatabaseSnafu {
            stage: "result-read-path",
            code: ApiCode::from(&*source),
        })?
        .context(RequestSnafu {
            stage: "job-result-find",
            code: ApiCode::not_found(4041001),
        })?;
    tracing::Span::current()
        .record("pdf_hash", tracing::field::display(&job.input_hash));
    if job.status == JobStatus::Failed {
        return RequestSnafu {
            stage: "job-result-failed",
            code: ApiCode::conflict(4091003),
        }
        .fail();
    }
    if job.status != JobStatus::Succeeded {
        return RequestSnafu {
            stage: "job-result-pending",
            code: ApiCode::conflict(4091002),
        }
        .fail();
    }
    let name = job.result_path.context(RequestSnafu {
        stage: "job-result-read-path",
        code: ApiCode::COMMON_INTERNAL_ERROR,
    })?;
    let file = tokio::fs::File::open(state.storage.path(&name)?)
        .await
        .context(StorageSnafu {
            stage: "result-open-file",
            code: ApiCode::service_unavailable(5031003),
        })?;
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}
