use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu},
    model::{
        base::ApiResponse,
        error::ApiErrorResponse,
        job::{JobQuery, JobSnapshot},
    },
    state::AppState,
};
use axum::extract::{Query, State, rejection::QueryRejection};
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use snafu::{OptionExt, ResultExt};

/// Retrieves current durable progress without requiring affinity to the accepting API instance.
#[utoipa::path(
    get, path = "/jobs/status", tag = super::TAG,
    params(JobQuery),
    responses(
        (status = 200, description = "Latest persisted task snapshot", body = ApiResponse<JobSnapshot>),
        (status = 400, description = "Invalid task UUID (4001002)", body = ApiErrorResponse),
        (status = 404, description = "Task not found (4041001)", body = ApiErrorResponse),
        (status = 503, description = "Task database unavailable (5031002)", body = ApiErrorResponse),
        (status = 500, description = "Internal error (500000)", body = ApiErrorResponse)
    )
)]
pub async fn status(
    State(state): State<AppState>,
    query: Result<Query<JobQuery>, QueryRejection>,
) -> ApiResult<ApiResponse<JobSnapshot>> {
    // Query extraction keeps task identifiers out of route templates and works with native EventSource.
    let Query(JobQuery { id }) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "job-status-parse-id",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    tracing::Span::current().record("job_id", tracing::field::display(id));
    let job = Jobs::find_by_id(&state.db, id)
        .await
        .with_context(|source| DatabaseSnafu {
            stage: "task-read-status",
            code: ApiCode::from(&*source),
        })?
        .context(RequestSnafu {
            stage: "job-status-find",
            code: ApiCode::not_found(4041001),
        })?;
    tracing::Span::current()
        .record("pdf_hash", tracing::field::display(&job.input_hash));
    Ok(ApiResponse::data(JobSnapshot::from(job)))
}
