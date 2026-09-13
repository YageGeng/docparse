use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// WisLand-compatible successful JSON envelope, also used inside SSE data messages.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ApiResponse<T> {
    pub data: T,
    pub success: bool,
    pub message: &'static str,
}

impl<T> ApiResponse<T> {
    /// Wraps typed data without forcing its HTTP status to 200 at the call site.
    pub fn data(data: T) -> Self {
        Self {
            data,
            success: true,
            message: "Success",
        }
    }
}

impl<T: Serialize> ApiResponse<T> {
    /// Streams the same derived envelope used by HTTP/SSE without buffering the complete response.
    pub fn write(
        &self,
        writer: impl std::io::Write,
    ) -> Result<(), serde_json::Error> {
        serde_json::to_writer(writer, self)
    }
}

impl<T: Serialize> IntoResponse for ApiResponse<T> {
    /// Serializes ordinary responses; callers may pair the envelope with 202 or other successful statuses.
    fn into_response(self) -> Response {
        axum::Json(self).into_response()
    }
}
