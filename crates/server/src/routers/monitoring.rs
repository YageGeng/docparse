//! Thin HTTP adapters; telemetry and Prometheus policy belong to the monitoring service.
use crate::{
    code::ApiCode,
    error::{ApiResult, RequestSnafu},
    model::{
        base::ApiResponse,
        monitoring::{HistoryQuery, Snapshot},
    },
    state::AppState,
};
use axum::{
    extract::{Query, State},
    http::header,
};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Registers monitoring endpoints under the same API prefix as other server routes.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(snapshot))
        .routes(routes!(history))
}

/// Exposes the standard scrape endpoint without making each scrape query PostgreSQL.
pub async fn metrics(
    State(state): State<AppState>,
) -> ApiResult<([(header::HeaderName, &'static str); 1], String)> {
    Ok((
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.monitoring()?.render(),
    ))
}

/// Converts the same recorder to JSON; never substitutes unavailable database data with zero.
#[utoipa::path(get, path = "/monitoring/snapshot", tag = "Monitoring", responses((status = 200, body = ApiResponse<Snapshot>)))]
pub async fn snapshot(
    State(state): State<AppState>,
) -> ApiResult<ApiResponse<Snapshot>> {
    Ok(ApiResponse::data(state.monitoring()?.snapshot()?))
}

/// Fetches bounded range results only from the configured upstream, preserving gaps and NaN strings.
#[utoipa::path(get, path = "/monitoring/history", tag = "Monitoring", params(HistoryQuery), responses((status = 200, body = ApiResponse<serde_json::Value>)))]
pub async fn history(
    State(state): State<AppState>,
    query: Result<
        Query<HistoryQuery>,
        axum::extract::rejection::QueryRejection,
    >,
) -> ApiResult<ApiResponse<serde_json::Value>> {
    let Query(query) = query.map_err(|_error| {
        RequestSnafu {
            stage: "metrics-history-query",
            code: ApiCode::COMMON_BAD_REQUEST,
        }
        .build()
    })?;
    Ok(ApiResponse::data(state.monitoring()?.history(query).await?))
}
