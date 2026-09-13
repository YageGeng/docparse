use axum::{Extension, Json};
use std::sync::Arc;
use utoipa::openapi::OpenApi;

/// Download the OpenAPI document generated from the registered handlers and response types.
#[utoipa::path(
    get, path = "/openapi.json", tag = super::TAG,
    responses((status = 200, description = "OpenAPI 3.1 document", body = Object, content_type = "application/json"))
)]
pub async fn openapi(
    Extension(document): Extension<Arc<OpenApi>>,
) -> Json<OpenApi> {
    Json((*document).clone())
}
