use docparse_server::model::base::ApiResponse;
use serde_json::json;

/// The trait drives both the HTTP status and the serialized Display message, including stage and source context.
#[tokio::test]
async fn error_response_uses_the_error_code_trait() {
    use axum::{body::to_bytes, response::IntoResponse};
    use docparse_server::{
        code::ApiCode,
        error::ApiError,
        model::error::{ApiErrorResponse, ErrorCode},
    };
    let error = ApiError::Storage {
        source: std::io::Error::other("disk busy"),
        stage: "result-open-file",
        code: ApiCode::service_unavailable(5031003),
    };
    let message = "file operation failed at result-open-file: disk busy";
    assert_eq!(ErrorCode::code(&error), 5031003);
    assert_eq!(ErrorCode::http_code(&error), 503);
    assert_eq!(ErrorCode::message(&error), message);
    let response = ApiErrorResponse::from(error).into_response();
    assert_eq!(response.status().as_u16(), 503);
    let body = to_bytes(response.into_body(), 4096).await.expect("body");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).expect("JSON"),
        json!({"success":false,"error":{"code":5031003,"message":message}})
    );
}

/// HTTP-category constructors contain only transport and business numbers and remain usable in const contexts.
#[test]
fn api_code_constructors_are_const_and_keep_their_status() {
    use docparse_server::code::ApiCode;
    const CODES: [ApiCode; 10] = [
        ApiCode::bad_request(123),
        ApiCode::not_found(123),
        ApiCode::method_not_allowed(123),
        ApiCode::request_timeout(123),
        ApiCode::conflict(123),
        ApiCode::payload_too_large(123),
        ApiCode::unprocessable_entity(123),
        ApiCode::too_many_requests(123),
        ApiCode::internal_error(123),
        ApiCode::service_unavailable(123),
    ];
    for (code, status) in CODES
        .into_iter()
        .zip([400, 404, 405, 408, 409, 413, 422, 429, 500, 503])
    {
        assert_eq!(code.http_code, status);
        assert_eq!(code.code, 123);
    }
}

/// File responses must use the same escaped envelope as ordinary API responses, including non-default fields.
#[test]
fn streamed_response_uses_the_serialized_response_contract() {
    let response = ApiResponse {
        data: json!({"text": "quoted \"value\"\n中文"}),
        success: false,
        message: "custom \"message\"\n",
    };
    let mut bytes = Vec::new();
    response.write(&mut bytes).expect("stream response");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .expect("valid JSON"),
        json!({"data":{"text":"quoted \"value\"\n中文"},"success":false,"message":"custom \"message\"\n"})
    );
}
