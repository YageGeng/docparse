use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu, StorageSnafu},
    model::error::ApiErrorResponse,
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Query, Request, State, rejection::QueryRejection},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use docparse_database::{JobStatus, query::parse_job::ParseJobQuery as Jobs};
use serde::Deserialize;
use snafu::{OptionExt, ResultExt};
use tower_http::services::ServeFile;
use uuid::Uuid;

/// Selects an absolute file image belonging to one completed result.
#[derive(Debug, Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct JobFigureQuery {
    /// Identifier of the completed parse task.
    pub id: Uuid,
    /// Absolute path recorded in the task's figure delivery.
    pub path: String,
}

/// Binary image bytes streamed directly in a non-JSON response.
pub struct FigureBinary;

impl utoipa::PartialSchema for FigureBinary {
    /// Describes the wire bytes as an OpenAPI binary string instead of a JSON array.
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{
            KnownFormat, ObjectBuilder, SchemaFormat, Type,
        };
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl utoipa::ToSchema for FigureBinary {}

/// Streams only images inside this task's retained figure directory.
#[utoipa::path(
    get, path = "/jobs/figure", tag = super::TAG,
    description = "Read a file-delivered figure owned by a completed task. Paths outside that task's figure directory are rejected.",
    params(JobFigureQuery),
    responses(
        (status = 200, description = "Figure image", content((FigureBinary = "image/png"), (FigureBinary = "image/jpeg"), (FigureBinary = "image/jp2"), (FigureBinary = "image/jpx"))),
        (status = 400, description = "Invalid or unowned image path", body = ApiErrorResponse),
        (status = 404, description = "Task not found", body = ApiErrorResponse),
        (status = 409, description = "Task is not complete", body = ApiErrorResponse),
        (status = 503, description = "Database or figure storage unavailable", body = ApiErrorResponse)
    )
)]
pub async fn figure(
    State(state): State<AppState>,
    query: Result<Query<JobFigureQuery>, QueryRejection>,
    request: Request,
) -> ApiResult<Response> {
    let Query(JobFigureQuery { id, path }) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "figure-parse-query",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    tracing::Span::current().record("job_id", tracing::field::display(id));
    let job = Jobs::find_by_id(&state.db, id)
        .await
        .with_context(|source| DatabaseSnafu {
            stage: "figure-find-job",
            code: ApiCode::from(&*source),
        })?
        .context(RequestSnafu {
            stage: "figure-find-job",
            code: ApiCode::not_found(4041001),
        })?;
    if job.status != JobStatus::Succeeded {
        return RequestSnafu {
            stage: "figure-check-job",
            code: ApiCode::conflict(4091002),
        }
        .fail();
    }
    let name = job.result_path.context(RequestSnafu {
        stage: "figure-result-path",
        code: ApiCode::COMMON_INTERNAL_ERROR,
    })?;
    let image = state.storage.figure_path(&name, &path).await?;
    // ServeFile streams the owned file without buffering image bytes in the API process.
    let mut response = ServeFile::new(image)
        .try_call(request)
        .await
        .context(StorageSnafu {
            stage: "figure-open-file",
            code: ApiCode::service_unavailable(5031003),
        })?
        .map(Body::new);
    if response.status() == StatusCode::NOT_FOUND {
        return RequestSnafu {
            stage: "figure-missing-file",
            code: ApiCode::not_found(4041001),
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
    Ok(response)
}
