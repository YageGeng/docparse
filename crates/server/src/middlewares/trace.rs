use axum::{
    body::{Body, Bytes, HttpBody},
    extract::Request,
    middleware::Next,
    response::Response,
};
use http_body::{Frame, SizeHint};
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};
use tracing::{Instrument, instrument::WithSubscriber};
use typed_builder::TypedBuilder;
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
        let head = method == axum::http::Method::HEAD;
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
        let status = response.status();
        let expected = response
            .headers()
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok());
        let (parts, body) = response.into_parts();
        let mut body = TransferBody::builder()
            .body(body)
            .started(started)
            .description(format!("{} {} response {}", method, path, status))
            .span(tracing::Span::current())
            .dispatcher(tracing::dispatcher::get_default(Clone::clone))
            .expected(expected)
            .build();
        // HEAD and no-body statuses may never be polled by the HTTP server.
        if head
            || body.body.is_end_stream()
            || status == axum::http::StatusCode::NOT_MODIFIED
        {
            body.finish("completed");
        }
        Response::from_parts(parts, Body::new(body))
    }
    .instrument(span)
    .with_current_subscriber()
    .await
}

/// Preserves body frames and size hints while observing bytes handed to the HTTP transport.
#[derive(TypedBuilder)]
struct TransferBody {
    body: Body,
    started: Instant,
    description: String,
    span: tracing::Span,
    dispatcher: tracing::Dispatch,
    #[builder(default)]
    bytes: u64,
    #[builder(default)]
    finished: bool,
    #[builder(default)]
    expected: Option<u64>,
}

impl TransferBody {
    /// Reports exactly one terminal outcome without logging response content or sensitive headers.
    fn finish(&mut self, outcome: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        tracing::dispatcher::with_default(&self.dispatcher, || {
            self.span.in_scope(|| {
                if outcome == "completed" {
                    tracing::info!(
                        "{} body completed: {} bytes in {} ms",
                        self.description,
                        self.bytes,
                        self.started.elapsed().as_millis()
                    );
                } else {
                    tracing::warn!(
                        "{} body {}: {} bytes in {} ms",
                        self.description,
                        outcome,
                        self.bytes,
                        self.started.elapsed().as_millis()
                    );
                }
            })
        });
    }
}

impl HttpBody for TransferBody {
    type Data = Bytes;
    type Error = axum::Error;

    /// Accounts for data only, preserving trailers and backpressure unchanged.
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let result = Pin::new(&mut self.body).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    self.bytes += bytes.len() as u64;
                }
                if self.body.is_end_stream()
                    || self.expected == Some(self.bytes)
                {
                    self.finish("completed");
                }
            }
            Poll::Ready(None) => self.finish("completed"),
            Poll::Ready(Some(Err(error))) => {
                tracing::dispatcher::with_default(&self.dispatcher, || {
                    self.span.in_scope(|| {
                        tracing::warn!(
                            "{} body read failed: {}",
                            self.description,
                            error
                        )
                    })
                });
                self.finish("failed");
            }
            Poll::Pending => {}
        }
        result
    }

    /// Retains empty-body behavior for HEAD and zero-length responses.
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    /// Keeps Content-Length inference available to Hyper and compression predicates.
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

impl Drop for TransferBody {
    /// Detects streams discarded before their final frame, including client disconnects.
    fn drop(&mut self) {
        self.finish("cancelled");
    }
}
