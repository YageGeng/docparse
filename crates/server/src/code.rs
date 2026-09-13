use axum::http::StatusCode;
use docparse_database::error::DatabaseError;

/// Stable business codes accompany HTTP status without exposing implementation errors to clients.
#[derive(Debug, Clone, Copy)]
pub struct ApiCode {
    pub http_code: u16,
    pub code: u64,
}

impl From<&DatabaseError> for ApiCode {
    /// Classifies persistence failures when a Snafu context is built, preserving conflicts before HTTP conversion.
    fn from(error: &DatabaseError) -> Self {
        match error {
            DatabaseError::IdempotencyConflict => Self::conflict(4091001),
            DatabaseError::InvalidInput | DatabaseError::Configuration(_) => {
                Self::COMMON_BAD_REQUEST
            }
            DatabaseError::SeaOrm(_) => Self::COMMON_DATABASE_ERROR,
        }
    }
}

impl ApiCode {
    // Keep only shared fallbacks as instances; business call sites choose their status and code.
    pub const COMMON_BAD_REQUEST: Self = Self::bad_request(400000);
    pub const COMMON_NOT_FOUND: Self = Self::not_found(404000);
    pub const COMMON_INTERNAL_ERROR: Self = Self::internal_error(500000);
    pub const COMMON_DATABASE_ERROR: Self = Self::service_unavailable(5031002);

    /// Constructs an HTTP bad request error with the caller's stable numeric code.
    pub const fn bad_request(code: u64) -> Self {
        Self {
            http_code: StatusCode::BAD_REQUEST.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP not found error with the caller's stable numeric code.
    pub const fn not_found(code: u64) -> Self {
        Self {
            http_code: StatusCode::NOT_FOUND.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP method not allowed error with the caller's stable numeric code.
    pub const fn method_not_allowed(code: u64) -> Self {
        Self {
            http_code: StatusCode::METHOD_NOT_ALLOWED.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP request timeout error with the caller's stable numeric code.
    pub const fn request_timeout(code: u64) -> Self {
        Self {
            http_code: StatusCode::REQUEST_TIMEOUT.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP conflict error with the caller's stable numeric code.
    pub const fn conflict(code: u64) -> Self {
        Self {
            http_code: StatusCode::CONFLICT.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP payload too large error with the caller's stable numeric code.
    pub const fn payload_too_large(code: u64) -> Self {
        Self {
            http_code: StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP unprocessable entity error with the caller's stable numeric code.
    pub const fn unprocessable_entity(code: u64) -> Self {
        Self {
            http_code: StatusCode::UNPROCESSABLE_ENTITY.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP too many requests error with the caller's stable numeric code.
    pub const fn too_many_requests(code: u64) -> Self {
        Self {
            http_code: StatusCode::TOO_MANY_REQUESTS.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP 500 error with the caller's stable numeric code.
    pub const fn internal_error(code: u64) -> Self {
        Self {
            http_code: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
            code,
        }
    }

    /// Constructs an HTTP service unavailable error with the caller's stable numeric code.
    pub const fn service_unavailable(code: u64) -> Self {
        Self {
            http_code: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            code,
        }
    }
}
