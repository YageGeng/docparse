use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu, StorageSnafu},
    model::{
        base::ApiResponse,
        error::ApiErrorResponse,
        job::{JobSnapshot, PdfUpload},
    },
    state::AppState,
};
use axum::{
    extract::{Multipart, OriginalUri, State, multipart::MultipartRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use snafu::{OptionExt, ResultExt};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Streams the sole PDF field to shared storage and acknowledges only a committed, idempotent task.
#[utoipa::path(
    post, path = "/jobs", tag = super::TAG,
    description = "Submit exactly one multipart file field containing a PDF. Idempotency-Key is also the job UUID: reuse it after a lost response. Identical content returns the existing visible job; different content or a deleted task returns 4091001. Input is persisted before HTTP 202. Upload size and timeout are deployment settings (defaults: 512 MiB and 300 seconds).",
    params(("Idempotency-Key" = Uuid, Header, description = "Required UUID identifying this submission and all safe retries")),
    request_body(content = PdfUpload, content_type = "multipart/form-data", description = "Exactly one binary file field"),
    responses(
        (status = 202, description = "Durable task accepted, or matching task already exists", body = ApiResponse<JobSnapshot>, headers(("Location" = String, description = "Task status URL"))),
        (status = 400, description = "Malformed multipart headers (400000), invalid upload (4001001), or invalid idempotency key (4001003)", body = ApiErrorResponse),
        (status = 408, description = "Upload timeout (4081001)", body = ApiErrorResponse),
        (status = 409, description = "Idempotency key refers to different PDF content or a deleted task (4091001)", body = ApiErrorResponse),
        (status = 413, description = "PDF or multipart request exceeds its configured limit (4131001)", body = ApiErrorResponse),
        (status = 429, description = "Upload capacity is full (4291001)", body = ApiErrorResponse),
        (status = 503, description = "Instance draining, database unavailable, or shared storage unavailable (5031001-5031003)", body = ApiErrorResponse),
        (status = 500, description = "Internal error (500000)", body = ApiErrorResponse)
    )
)]
pub async fn upload(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<Response> {
    // Convert extractor rejections at entry; completed responses are never inspected to invent an APICODE.
    let mut multipart = multipart.map_err(|_rejection| {
        RequestSnafu {
            stage: "upload-read-boundary",
            code: ApiCode::COMMON_BAD_REQUEST,
        }
        .build()
    })?;
    if state.shutdown.is_cancelled() {
        return RequestSnafu {
            stage: "upload-check-drain",
            code: ApiCode::service_unavailable(5031001),
        }
        .fail();
    }
    let id = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .context(RequestSnafu {
            stage: "upload-parse-key",
            code: ApiCode::bad_request(4001003),
        })?;
    // A task's UUID is known before reading the PDF; its content hash becomes available after the stream completes.
    tracing::Span::current().record("job_id", tracing::field::display(id));
    let _permit = Arc::clone(&state.uploads).try_acquire_owned().map_err(
        |_capacity| {
            RequestSnafu {
                stage: "upload-acquire-slot",
                code: ApiCode::too_many_requests(4291001),
            }
            .build()
        },
    )?;
    let task = async {
        let temporary = state.storage.temporary().await?;
        let file = temporary.as_file().try_clone().context(StorageSnafu {
            stage: "upload-open-writer",
            code: ApiCode::service_unavailable(5031003),
        })?;
        let mut writer = tokio::fs::File::from_std(file);
        let mut hash = blake3::Hasher::new();
        let mut total = 0usize;
        let mut prefix = Vec::<u8>::with_capacity(5);
        let mut seen = false;
        let mut filename = None;
        // Field headers and streamed chunks share the same From<MultipartError> boundary.
        while let Some(mut field) = multipart.next_field().await? {
            if seen || field.name() != Some("file") {
                return RequestSnafu {
                    stage: "upload-check-field",
                    code: ApiCode::bad_request(4001001),
                }
                .fail();
            }
            seen = true;
            // Keep only a bounded display name; storage continues to use the content hash exclusively.
            filename = field
                .file_name()
                .map(|name| {
                    name.rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(name)
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(255)
                        .collect::<String>()
                })
                .filter(|name| !name.trim().is_empty());
            while let Some(chunk) = field.chunk().await? {
                total =
                    total.checked_add(chunk.len()).context(RequestSnafu {
                        stage: "upload-count-bytes",
                        code: ApiCode::payload_too_large(4131001),
                    })?;
                if total > state.options.max_upload_bytes {
                    return RequestSnafu {
                        stage: "upload-check-size",
                        code: ApiCode::payload_too_large(4131001),
                    }
                    .fail();
                }
                prefix.extend(chunk.iter().take(5 - prefix.len()));
                hash.update(&chunk);
                writer.write_all(&chunk).await.context(StorageSnafu {
                    stage: "upload-write-pdf",
                    code: ApiCode::service_unavailable(5031003),
                })?;
            }
        }
        if !seen || prefix != b"%PDF-" {
            return RequestSnafu {
                stage: "upload-check-signature",
                code: ApiCode::bad_request(4001001),
            }
            .fail();
        }
        writer.flush().await.context(StorageSnafu {
            stage: "upload-flush-pdf",
            code: ApiCode::service_unavailable(5031003),
        })?;
        drop(writer);
        let hash = hash.finalize().to_hex().to_string();
        tracing::Span::current()
            .record("pdf_hash", tracing::field::display(&hash));
        state
            .storage
            .publish(temporary, &format!("{hash}.pdf"))
            .await?;
        let size = i64::try_from(total).map_err(|_overflow| {
            RequestSnafu {
                stage: "upload-convert-size",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            }
            .build()
        })?;
        let job =
            Jobs::submit(&state.db, id, &hash, filename.as_deref(), Some(size))
                .await
                .with_context(|source| DatabaseSnafu {
                    stage: "task-submit-pdf",
                    code: ApiCode::from(&*source),
                })?;
        tracing::info!("accepted PDF job {} with {} bytes", id, total);
        // OriginalUri retains the final API prefix without copying routing configuration into application state.
        Ok((
            StatusCode::ACCEPTED,
            [(header::LOCATION, format!("{}/status?id={id}", uri.path()))],
            ApiResponse::data(JobSnapshot::from(job)),
        )
            .into_response())
    };
    tokio::time::timeout(state.options.upload_timeout, task)
        .await
        .map_err(|_elapsed| {
            RequestSnafu {
                stage: "upload-wait-body",
                code: ApiCode::request_timeout(4081001),
            }
            .build()
        })?
}
