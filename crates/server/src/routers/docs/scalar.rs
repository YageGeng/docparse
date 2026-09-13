use axum::{Extension, response::Html};
use std::sync::Arc;
use utoipa::openapi::OpenApi;
use utoipa_scalar::Scalar;

/// Browse and try the API using Scalar with the same generated OpenAPI document.
#[utoipa::path(
    get, path = "/docs", tag = super::TAG,
    responses((status = 200, description = "Scalar API reference", body = String, content_type = "text/html"))
)]
pub async fn scalar(
    Extension(document): Extension<Arc<OpenApi>>,
) -> Html<String> {
    Html(Scalar::new((*document).clone()).to_html())
}
