use crate::{
    cleanup::DeletedResults,
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu},
    model::{base::ApiResponse, error::ApiErrorResponse, job::JobQuery},
    state::AppState,
};
use axum::{
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
};
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use snafu::ResultExt;
use uuid::Uuid;

/// Removes a completed task and its result while retaining the shared original PDF and internal cursor anchor.
#[utoipa::path(
    post, path = "/jobs/delete", tag = super::TAG,
    description = "Persist deletion of a succeeded or failed task before removing its result JSON. Return 200 after cleanup or 202 while durable cleanup remains pending. Every server role retries cleanup after restart and periodically. Preserve the shared original PDF. Repeated deletion succeeds; queued/running tasks cannot be deleted.",
    params(JobQuery),
    responses(
        (status = 200, description = "Deleted task UUID", body = ApiResponse<Uuid>),
        (status = 202, description = "Task deleted; result cleanup will be retried automatically", body = ApiResponse<Uuid>),
        (status = 400, description = "Invalid task UUID (4001002)", body = ApiErrorResponse),
        (status = 404, description = "Task not found (4041001)", body = ApiErrorResponse),
        (status = 409, description = "Task is still queued or running (4091004)", body = ApiErrorResponse),
        (status = 503, description = "Deletion intent could not be confirmed in the database (5031002)", body = ApiErrorResponse)
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    query: Result<Query<JobQuery>, QueryRejection>,
) -> ApiResult<(StatusCode, ApiResponse<Uuid>)> {
    let Query(JobQuery { id }) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "job-delete-parse-id",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    tracing::Span::current().record("job_id", tracing::field::display(id));
    // Commit intent first: an interrupted cleanup must never restore a successful task with a missing file.
    let job =
        Jobs::mark_deleted(&state.db, id)
            .await
            .with_context(|source| DatabaseSnafu {
                stage: "job-delete-mark",
                code: ApiCode::from(&*source),
            })?;
    let Some(job) = job else {
        let exists = Jobs::find_by_id(&state.db, id)
            .await
            .with_context(|source| DatabaseSnafu {
                stage: "job-delete-find",
                code: ApiCode::from(&*source),
            })?
            .is_some();
        return RequestSnafu {
            stage: "job-delete-check-status",
            code: if exists {
                ApiCode::conflict(4091004)
            } else {
                ApiCode::not_found(4041001)
            },
        }
        .fail();
    };
    tracing::Span::current()
        .record("pdf_hash", tracing::field::display(&job.input_hash));
    let status = match DeletedResults::from(&state).clean(&job).await {
        Ok(()) => StatusCode::OK,
        Err(error) => {
            tracing::warn!(
                "result cleanup for deleted job {} remains pending: {}",
                id,
                error
            );
            StatusCode::ACCEPTED
        }
    };
    tracing::info!("deleted result for job {}", id);
    Ok((status, ApiResponse::data(id)))
}
