use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu},
    model::{base::ApiResponse, error::ApiErrorResponse},
    state::AppState,
};
use axum::extract::State;
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use snafu::ResultExt;

/// Allows a load balancer to stop routing to draining instances or unavailable shared dependencies.
#[utoipa::path(
    get, path = "/ready", tag = super::TAG,
    description = "Check task schema availability, writable shared storage, and whether this instance is draining. API-only instances do not load models or verify that a remote worker is available.",
    responses(
        (status = 200, description = "Shared dependencies ready", body = ApiResponse<String>, example = json!({"data":"ready","success":true,"message":"Success"})),
        (status = 503, description = "Instance draining, database/schema unavailable, or shared storage unavailable (5031001-5031003)", body = ApiErrorResponse),
        (status = 500, description = "Internal readiness error (500000)", body = ApiErrorResponse)
    )
)]
pub async fn ready(
    State(state): State<AppState>,
) -> ApiResult<ApiResponse<&'static str>> {
    // Readiness failures declare their public business code at the point where the condition is detected.
    if state.shutdown.is_cancelled() {
        return RequestSnafu {
            stage: "http-check-drain",
            code: ApiCode::service_unavailable(5031001),
        }
        .fail();
    }
    // Connectivity alone cannot serve jobs when the generated task schema is missing.
    Jobs::ready(&state.db)
        .await
        .with_context(|source| DatabaseSnafu {
            stage: "db-check-ready",
            code: ApiCode::from(&*source),
        })?;
    state.storage.ready().await?;
    Ok(ApiResponse::data("ready"))
}
