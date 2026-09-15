use crate::{
    code::ApiCode,
    error::{
        ApiResult, DatabaseSnafu, RequestSnafu, SerializeSnafu, StorageSnafu,
        TaskSnafu,
    },
    model::{
        base::ApiResponse,
        error::ApiErrorResponse,
        job::{JobResultQuery, ResultFormat},
    },
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
    description = "Read the completed document as JSON (default) or Markdown. JSON includes both LaTeX and Markdown for every recognized formula. Markdown is a presentation of the stored result; no inference is repeated.",
    params(JobResultQuery),
    responses(
        (status = 200, description = "Canonical JSON envelope or complete document Markdown", content((ApiResponse<DocumentResult> = "application/json"), (String = "text/markdown"))),
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
) -> ApiResult<Response> {
    // Query extraction keeps task identifiers out of route templates and works with native EventSource.
    let Query(JobResultQuery { id, format }) = query.map_err(|_rejection| {
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
    if matches!(format.unwrap_or_default(), ResultFormat::Markdown) {
        /// Only the canonical document is needed from the persisted success envelope.
        #[derive(serde::Deserialize)]
        struct StoredDocument {
            data: DocumentResult,
        }
        let file = file.into_std().await;
        let placeholder = state.options.output.formula_placeholder.clone();
        tracing::info!("rendering stored job {} as Markdown", id);
        let markdown =
            tokio::task::spawn_blocking(move || -> ApiResult<String> {
                let stored: StoredDocument =
                    serde_json::from_reader(std::io::BufReader::new(file))
                        .context(SerializeSnafu {
                            stage: "result-decode-json",
                            code: ApiCode::COMMON_INTERNAL_ERROR,
                        })?;
                Ok(docparse_core::MarkdownRenderer::new(
                    docparse_core::RenderView::Semantic,
                    placeholder,
                )
                .render(&stored.data))
            })
            .await
            .context(TaskSnafu {
                stage: "result-render-markdown",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            })??;
        tracing::info!(
            "rendered stored job {} as {} Markdown bytes",
            id,
            markdown.len()
        );
        return Ok((
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            markdown,
        )
            .into_response());
    }
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}
