use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Separates error identity and transport status from the error's own Display-based explanation.
pub trait ErrorCode {
    /// Returns the stable business error number.
    fn code(&self) -> u64;
    /// Returns the error's diagnostic message, including its stage when available.
    fn message(&self) -> String;
    /// Returns the HTTP status used for this error.
    fn http_code(&self) -> u16;
}

/// Stable failure shape shared with WisLand's clients.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ApiErrorResponse {
    pub success: bool,
    pub error: ErrorDetail,
    // Transport metadata must not add fields to the JSON envelope or OpenAPI schema.
    #[serde(skip)]
    http_code: u16,
}

/// Response details carry the numeric code and the error's own formatted explanation.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorDetail {
    pub code: u64,
    pub message: String,
}

impl<T: ErrorCode> From<T> for ApiErrorResponse {
    /// Builds the same envelope for HTTP responses and SSE events from the shared error contract.
    fn from(error: T) -> Self {
        Self {
            success: false,
            error: ErrorDetail {
                code: error.code(),
                message: error.message(),
            },
            http_code: error.http_code(),
        }
    }
}

impl IntoResponse for ApiErrorResponse {
    /// Applies the trait-provided status while preventing malformed custom statuses from panicking.
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.http_code)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, axum::Json(self)).into_response()
    }
}
