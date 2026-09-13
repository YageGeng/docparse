use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu, StorageSnafu},
    model::{error::ApiErrorResponse, job::JobQuery},
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Query, Request, State, rejection::QueryRejection},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use snafu::{OptionExt, ResultExt};
use tower_http::services::ServeFile;

/// Streams the immutable uploaded PDF with standard HEAD and byte-range support for lazy browser previews.
#[utoipa::path(
    get, path = "/jobs/source", tag = super::TAG,
    description = "Read the original uploaded PDF independently of parse status. Supports HEAD, single byte ranges and conditional requests through the file service. Storage paths never derive from the uploaded filename.",
    params(JobQuery, ("Range" = Option<String>, Header, description = "Optional byte range, for example bytes=0-65535")),
    responses(
        (status = 200, description = "Original PDF", body = String, content_type = "application/pdf"),
        (status = 206, description = "Requested PDF byte range", body = String, content_type = "application/pdf"),
        (status = 304, description = "Conditional request is not modified"),
        (status = 400, description = "Invalid task UUID (4001002)", body = ApiErrorResponse),
        (status = 404, description = "Task not found (4041001)", body = ApiErrorResponse),
        (status = 416, description = "Byte range not satisfiable; Content-Range reports the PDF size"),
        (status = 503, description = "Database or source file unavailable (5031002-5031003)", body = ApiErrorResponse)
    )
)]
pub async fn source(
    State(state): State<AppState>,
    query: Result<Query<JobQuery>, QueryRejection>,
    request: Request,
) -> ApiResult<Response> {
    let Query(JobQuery { id }) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "job-source-parse-id",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    tracing::Span::current().record("job_id", tracing::field::display(id));
    let job = Jobs::find_by_id(&state.db, id)
        .await
        .with_context(|source| DatabaseSnafu {
            stage: "job-source-find",
            code: ApiCode::from(&*source),
        })?
        .context(RequestSnafu {
            stage: "job-source-find",
            code: ApiCode::not_found(4041001),
        })?;
    tracing::Span::current()
        .record("pdf_hash", tracing::field::display(&job.input_hash));
    let path = state.storage.path(&format!("{}.pdf", job.input_hash))?;
    // Delegate range parsing and bounded streaming to tower-http instead of buffering or implementing HTTP ranges ourselves.
    let mut response = ServeFile::new(path)
        .try_call(request)
        .await
        .context(StorageSnafu {
            stage: "job-source-open",
            code: ApiCode::service_unavailable(5031003),
        })?
        .map(Body::new);
    if response.status() == StatusCode::NOT_FOUND {
        return RequestSnafu {
            stage: "job-source-missing",
            code: ApiCode::service_unavailable(5031003),
        }
        .fail();
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"document.pdf\""),
    );
    Ok(response)
}
