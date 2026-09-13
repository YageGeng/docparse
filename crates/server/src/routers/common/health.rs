use crate::model::base::ApiResponse;

/// Reports process liveness using the same ordinary JSON response shape.
#[utoipa::path(
    get, path = "/health", tag = super::TAG,
    responses((status = 200, description = "Process is alive", body = ApiResponse<String>, example = json!({"data":"ok","success":true,"message":"Success"})))
)]
pub async fn health() -> ApiResponse<&'static str> {
    ApiResponse::data("ok")
}
