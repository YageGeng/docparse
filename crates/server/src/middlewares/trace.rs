use axum::{extract::Request, middleware::Next, response::Response};
use tracing::{Instrument, instrument::WithSubscriber};
use uuid::Uuid;

/// Correlates HTTP work while leaving bodies, headers, and query parameters out of logs.
pub async fn request_trace(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    // Request identity is separate from the durable job and content identities filled by job handlers.
    let span = tracing::info_span!(target: crate::logging::CONTEXT_TARGET, "http_request", request_id = %request_id,
        job_id = tracing::field::Empty, pdf_hash = tracing::field::Empty);
    async move {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let started = std::time::Instant::now();
        let mut response = next.run(request).await;
        tracing::debug!(
            "{} {} response ready: {} in {} ms",
            method,
            path,
            response.status(),
            started.elapsed().as_millis()
        );
        if let Ok(value) = request_id.parse() {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
    .instrument(span)
    .with_current_subscriber()
    .await
}
