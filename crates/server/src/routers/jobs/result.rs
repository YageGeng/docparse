use crate::{
    code::ApiCode,
    error::{
        ApiResult, DatabaseSnafu, RequestSnafu, SerializeSnafu, StorageSnafu,
    },
    model::{
        base::ApiResponse,
        error::ApiErrorResponse,
        job::{JobJsonResult, JobResultQuery, ResultFormat},
    },
    service::result_files::ResultIndex,
    state::AppState,
};
use axum::{
    body::{Body, Bytes},
    extract::{Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use docparse_database::{JobStatus, query::parse_job::ParseJobQuery as Jobs};
use futures_util::{StreamExt, stream};
use snafu::{OptionExt, ResultExt};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

/// Streams the immutable successful JSON envelope from shared storage rather than loading it into API memory.
#[utoipa::path(
    get, path = "/jobs/result", tag = super::TAG,
    description = "Read the completed document as JSON (default), one JSON page with page=N, or cached Markdown. Responses support gzip and If-None-Match revalidation. JSON includes both LaTeX and Markdown for every recognized formula. Markdown is a presentation of the stored result; no inference is repeated.",
    params(JobResultQuery, ("If-None-Match" = Option<String>, Header, description = "Revalidate an immutable result representation")),
    responses(
        (status = 200, description = "Canonical JSON envelope or complete document Markdown", content((ApiResponse<JobJsonResult> = "application/json"), (String = "text/markdown"))),
        (status = 304, description = "Result representation is unchanged"),
        (status = 400, description = "Invalid task UUID (4001002)", body = ApiErrorResponse),
        (status = 404, description = "Task not found (4041001)", body = ApiErrorResponse),
        (status = 409, description = "Result not ready (4091002) or task failed (4091003)", body = ApiErrorResponse),
        (status = 503, description = "Database or shared storage unavailable (5031002-5031003)", body = ApiErrorResponse),
        (status = 500, description = "Internal result state error (500000)", body = ApiErrorResponse)
    )
)]
pub async fn result(
    State(state): State<AppState>,
    query: Result<Query<JobResultQuery>, QueryRejection>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // Query extraction keeps task identifiers out of route templates and works with native EventSource.
    let Query(JobResultQuery { id, format, page }) =
        query.map_err(|_rejection| {
            RequestSnafu {
                stage: "job-result-parse-id",
                code: ApiCode::bad_request(4001002),
            }
            .build()
        })?;
    let markdown = matches!(format.unwrap_or_default(), ResultFormat::Markdown);
    let page = page.map(u32::from);
    if page.is_some() && markdown {
        return RequestSnafu {
            stage: "result-check-page",
            code: ApiCode::bad_request(4001002),
        }
        .fail();
    }
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
    let source = state.storage.path(&name)?;
    // Check durable visibility and source existence before honoring validators, including wildcard requests.
    let mut file =
        tokio::fs::File::open(&source).await.context(StorageSnafu {
            stage: "result-open-file",
            code: ApiCode::service_unavailable(5031003),
        })?;
    let index = if page.is_some() {
        let index = state.storage.result_artifact(&name, None).await?;
        let bytes = tokio::fs::read(index).await.context(StorageSnafu {
            stage: "result-read-index",
            code: ApiCode::service_unavailable(5031003),
        })?;
        let index: ResultIndex =
            serde_json::from_slice(&bytes).context(SerializeSnafu {
                stage: "result-decode-index",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            })?;
        if page.is_some_and(|number| number > index.page_count) {
            return RequestSnafu {
                stage: "result-check-page",
                code: ApiCode::bad_request(4001002),
            }
            .fail();
        }
        Some(index)
    } else {
        None
    };
    // Weak validators identify the semantic representation across gzip and identity content codings.
    let identity = format!(
        "{}:{name}:{page:?}:{markdown}:{}",
        // Match the algorithm-fence cache revision so clients cannot revalidate an obsolete presentation.
        if markdown { "v3" } else { "v1" },
        if markdown {
            state.options.output.formula_placeholder.as_str()
        } else {
            ""
        }
    );
    let tag = format!("\"{}\"", blake3::hash(identity.as_bytes()));
    let etag = format!("W/{tag}");
    let unchanged = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| {
            let value = value.trim();
            value == "*" || value.strip_prefix("W/").unwrap_or(value) == tag
        });
    let mut response = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else if let (Some(number), Some(index)) = (page, index) {
        // Only the selected byte range enters the HTTP body; canonical page JSON is never decoded or copied here.
        let prefix = format!(
            "{{\"data\":{{\"page_count\":{},\"errors\":{},\"page\":",
            index.page_count,
            serde_json::to_string(&index.errors).context(SerializeSnafu {
                stage: "result-encode-errors",
                code: ApiCode::COMMON_INTERNAL_ERROR
            })?
        );
        let suffix =
            Bytes::from_static(b"},\"success\":true,\"message\":\"Success\"}");
        let body = if let Some(range) = index.pages.get(&number) {
            file.seek(std::io::SeekFrom::Start(range.start))
                .await
                .context(StorageSnafu {
                    stage: "result-seek-page",
                    code: ApiCode::service_unavailable(5031003),
                })?;
            Body::from_stream(
                stream::once(async {
                    Ok::<_, std::io::Error>(Bytes::from(prefix))
                })
                .chain(ReaderStream::with_capacity(
                    file.take(range.end - range.start),
                    256 * 1024,
                ))
                .chain(stream::once(async { Ok::<_, std::io::Error>(suffix) })),
            )
        } else {
            Body::from(format!(
                "{prefix}null{}",
                String::from_utf8_lossy(&suffix)
            ))
        };
        ([(header::CONTENT_TYPE, "application/json")], body).into_response()
    } else {
        let content_type = if markdown {
            let path = state
                .storage
                .result_artifact(
                    &name,
                    Some(&state.options.output.formula_placeholder),
                )
                .await?;
            file = tokio::fs::File::open(path).await.context(StorageSnafu {
                stage: "result-open-markdown",
                code: ApiCode::service_unavailable(5031003),
            })?;
            "text/markdown; charset=utf-8"
        } else {
            "application/json"
        };
        let length = file
            .metadata()
            .await
            .context(StorageSnafu {
                stage: "result-read-size",
                code: ApiCode::service_unavailable(5031003),
            })?
            .len();
        (
            [
                (header::CONTENT_TYPE, content_type.to_owned()),
                (header::CONTENT_LENGTH, length.to_string()),
            ],
            Body::from_stream(ReaderStream::with_capacity(file, 256 * 1024)),
        )
            .into_response()
    };
    response
        .headers_mut()
        .insert(header::ETAG, etag.parse().expect("generated ASCII ETag"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "private, no-cache".parse().expect("static cache policy"),
    );
    response.headers_mut().insert(
        header::VARY,
        "Accept-Encoding".parse().expect("static Vary"),
    );
    Ok(response)
}
