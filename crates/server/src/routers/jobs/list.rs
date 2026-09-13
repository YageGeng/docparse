use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu},
    model::{
        base::ApiResponse,
        error::ApiErrorResponse,
        job::{JobList, JobListQuery, JobSnapshot},
    },
    state::AppState,
};
use axum::extract::{Query, State, rejection::QueryRejection};
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use snafu::ResultExt;

/// Lists durable tasks by descending creation time with bounded cursor pagination and optional filters.
#[utoipa::path(
    get, path = "/jobs/list", tag = super::TAG,
    description = "List persisted tasks newest first. Limit defaults to 20 and must be 1–100. Pass next_cursor with the same filters to continue. Search matches literal filename text case-insensitively. Old jobs can have null filename and size_bytes.",
    params(JobListQuery),
    responses(
        (status = 200, description = "Durable task history", body = ApiResponse<JobList>),
        (status = 400, description = "Invalid query or cursor (4001002)", body = ApiErrorResponse),
        (status = 503, description = "Task database unavailable (5031002)", body = ApiErrorResponse)
    )
)]
pub async fn list(
    State(state): State<AppState>,
    query: Result<Query<JobListQuery>, QueryRejection>,
) -> ApiResult<ApiResponse<JobList>> {
    let Query(query) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "job-list-parse-query",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    let limit = query.limit.unwrap_or(20);
    if !(1..=100).contains(&limit)
        || query
            .search
            .as_ref()
            .is_some_and(|search| search.chars().count() > 200)
    {
        return RequestSnafu {
            stage: "job-list-check-query",
            code: ApiCode::bad_request(4001002),
        }
        .fail();
    }
    let mut rows = Jobs::list(
        &state.db,
        query.cursor,
        limit,
        query.status,
        query.search.as_deref(),
    )
    .await
    .with_context(|source| DatabaseSnafu {
        stage: "job-list-read",
        code: match source {
            docparse_database::error::DatabaseError::InvalidInput => {
                ApiCode::bad_request(4001002)
            }
            source => ApiCode::from(&*source),
        },
    })?;
    // Fetch one extra row to distinguish a full final page from a page with more history.
    let next_cursor = if rows.len() > limit as usize {
        rows.pop();
        rows.last().map(|job| job.id)
    } else {
        None
    };
    Ok(ApiResponse::data(JobList {
        items: rows.into_iter().map(JobSnapshot::from).collect(),
        next_cursor,
    }))
}
