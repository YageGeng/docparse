use crate::{
    code::ApiCode,
    model::error::{ApiErrorResponse, ErrorCode},
};
use axum::{
    extract::multipart::MultipartError,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use docparse_database::error::DatabaseError;
use snafu::Snafu;

pub type ApiResult<T> = Result<T, ApiError>;

/// Every server-only Snafu variant carries the caller-selected APICODE alongside its diagnostic source.
/// Stage fields locate each failure; Display supplies the shared HTTP, SSE, and persisted message.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum ApiError {
    #[snafu(display("request failed at {stage}"))]
    Request { stage: &'static str, code: ApiCode },
    #[snafu(display("database operation failed at {stage}: {source}"))]
    Database {
        #[snafu(source(from(DatabaseError, Box::new)))]
        source: Box<DatabaseError>,
        stage: &'static str,
        code: ApiCode,
    },
    #[snafu(display("file operation failed at {stage}: {source}"))]
    Storage {
        source: std::io::Error,
        stage: &'static str,
        code: ApiCode,
    },
    #[snafu(display("task failed at {stage}: {source}"))]
    Task {
        source: tokio::task::JoinError,
        stage: &'static str,
        code: ApiCode,
    },
    #[snafu(display("serialization failed at {stage}: {source}"))]
    Serialize {
        source: serde_json::Error,
        stage: &'static str,
        code: ApiCode,
    },
    #[snafu(display("parser failed at {stage}: {source}"))]
    Parse {
        #[snafu(source(from(docparse_core::DocParseError, Box::new)))]
        source: Box<docparse_core::DocParseError>,
        stage: &'static str,
        code: ApiCode,
    },
    #[snafu(display("multipart failed at {stage}: {source}"))]
    Multipart {
        source: MultipartError,
        stage: &'static str,
        code: ApiCode,
    },
}

impl ApiError {
    /// Returns the stored code without reclassifying the error at the response boundary.
    pub fn code(&self) -> ApiCode {
        match self {
            Self::Request { code, .. }
            | Self::Database { code, .. }
            | Self::Storage { code, .. }
            | Self::Task { code, .. }
            | Self::Serialize { code, .. }
            | Self::Parse { code, .. }
            | Self::Multipart { code, .. } => *code,
        }
    }
}

impl ErrorCode for ApiError {
    /// Reads the business number carried by the error variant.
    fn code(&self) -> u64 {
        self.code().code
    }

    /// Uses the error's own Display implementation instead of storing text in ApiCode.
    fn message(&self) -> String {
        self.to_string()
    }

    /// Reads the HTTP status independently of the business number.
    fn http_code(&self) -> u16 {
        self.code().http_code
    }
}

impl From<MultipartError> for ApiError {
    /// Applies one multipart rejection policy for both field headers and streamed file chunks.
    fn from(error: MultipartError) -> Self {
        // Preserve the original body-read failure now that Display is the response's source of text.
        let code = if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiCode::payload_too_large(4131001)
        } else {
            ApiCode::bad_request(4001001)
        };
        Self::Multipart {
            source: error,
            stage: "upload-read-multipart",
            code,
        }
    }
}

impl IntoResponse for ApiError {
    /// Serializes the carried APICODE directly so handlers and rejections need no response-wrapping middleware.
    fn into_response(self) -> Response {
        let code = self.code();
        if (500..600).contains(&code.http_code) {
            tracing::error!(
                "request failed with API code {}: {}",
                code.code,
                self
            );
        }
        ApiErrorResponse::from(self).into_response()
    }
}

/// Routes caught panics through the same typed error response without exposing the panic payload.
#[derive(Clone)]
pub struct PanicHandler;

impl tower_http::catch_panic::ResponseForPanic for PanicHandler {
    type ResponseBody = axum::body::Body;

    /// Keeps panic handling at the error boundary instead of rewriting completed responses.
    fn response_for_panic(
        &mut self,
        _payload: Box<dyn std::any::Any + Send + 'static>,
    ) -> Response {
        ApiError::Request {
            stage: "http-handler-panic",
            code: ApiCode::COMMON_INTERNAL_ERROR,
        }
        .into_response()
    }
}
