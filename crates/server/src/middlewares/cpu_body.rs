//! Offload negotiated compression independently of parser CPU admission and preserve precompressed I/O.
use axum::{
    body::{Body, Bytes, HttpBody},
    extract::Request,
    middleware::Next,
    response::Response,
};
use docparse_common::{TaskError, WasmBoxedFuture};
use futures_util::task::AtomicWaker;
use http_body::{Frame, SizeHint};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

type FramePoll = Poll<Option<Result<Frame<Bytes>, axum::Error>>>;
type PollTask = WasmBoxedFuture<'static, Result<(Body, FramePoll), TaskError>>;

// ponytail: one compression poll at a time leaves blocking I/O headroom; widen only after measuring response CPU contention.
static COMPRESSION: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(1);

/// Identifies representations already encoded by the source service before compression middleware runs.
#[derive(Clone, Copy)]
struct Precompressed;

/// A wake can race the CPU reply; retain it until the next inner poll consumes it.
#[derive(Default)]
struct BodyWake {
    ready: AtomicBool,
    caller: AtomicWaker,
}

impl Wake for BodyWake {
    /// Publishes readiness before waking the HTTP task, including early wakes during compression.
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    /// Avoids losing readiness when the blocking job has not returned the body yet.
    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, Ordering::Release);
        self.caller.wake();
    }
}

/// Owns at most one body poll and one returned frame; slow clients never pin a blocking worker.
struct CpuBody {
    body: Option<Body>,
    polling: Option<PollTask>,
    wake: Arc<BodyWake>,
}

impl From<Body> for CpuBody {
    /// Schedules the first frame only when the transport actually requests it.
    fn from(body: Body) -> Self {
        Self {
            body: Some(body),
            polling: None,
            wake: Arc::new(BodyWake {
                ready: AtomicBool::new(true),
                caller: AtomicWaker::new(),
            }),
        }
    }
}

impl HttpBody for CpuBody {
    type Data = Bytes;
    type Error = axum::Error;

    /// Moves one compression poll to bounded HTTP blocking work, independently of parser CPU slots.
    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> FramePoll {
        self.wake.caller.register(cx.waker());
        if self.polling.is_none() {
            if self.body.is_none() {
                return Poll::Ready(None);
            }
            if !self.wake.ready.swap(false, Ordering::AcqRel) {
                return Poll::Pending;
            }
            let mut body =
                self.body.take().expect("body is present before polling");
            let waker = Waker::from(Arc::clone(&self.wake));
            self.polling = Some(Box::pin(async move {
                let permit = COMPRESSION
                    .acquire()
                    .await
                    .expect("compression admission stays open");
                docparse_common::run_blocking(move || {
                    // Release at the end of this poll, before waiting for a slow client to consume the frame.
                    let _permit = permit;
                    let frame = Pin::new(&mut body)
                        .poll_frame(&mut Context::from_waker(&waker));
                    (body, frame)
                })
                .await
            }));
        }
        let completed = std::task::ready!(
            self.polling
                .as_mut()
                .expect("poll in progress")
                .as_mut()
                .poll(cx)
        );
        self.polling = None;
        match completed {
            Ok((body, frame)) => {
                if !matches!(frame, Poll::Ready(None)) {
                    self.body = Some(body);
                    if frame.is_ready() {
                        self.wake.ready.store(true, Ordering::Release);
                    } else if self.wake.ready.load(Ordering::Acquire) {
                        // The inner source woke before its body returned; another poll must observe that wake.
                        cx.waker().wake_by_ref();
                    }
                }
                frame
            }
            Err(error) => {
                tracing::warn!(
                    "compressed response CPU task failed: {}",
                    error
                );
                Poll::Ready(Some(Err(axum::Error::new(error))))
            }
        }
    }

    /// Retains a known end marker when no compression poll is in flight.
    fn is_end_stream(&self) -> bool {
        self.polling.is_none()
            && self.body.as_ref().is_none_or(HttpBody::is_end_stream)
    }

    /// Compression normally has no exact size; preserve any hint available between polls.
    fn size_hint(&self) -> SizeHint {
        self.body
            .as_ref()
            .map_or_else(SizeHint::default, HttpBody::size_hint)
    }
}

/// Records source encodings inside the compression layer so they retain their ordinary I/O path.
pub async fn mark_precompressed(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if response
        .headers()
        .contains_key(axum::http::header::CONTENT_ENCODING)
    {
        response.extensions_mut().insert(Precompressed);
    }
    response
}

/// Wraps negotiated compression after the compression layer has built its response.
pub async fn offload_compression(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if response
        .headers()
        .contains_key(axum::http::header::CONTENT_ENCODING)
        && response.extensions().get::<Precompressed>().is_none()
    {
        response.map(|body| Body::new(CpuBody::from(body)))
    } else {
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Already encoded sources must bypass admission even when response compression itself is busy.
    #[tokio::test]
    async fn precompressed_responses_bypass_compression_admission() {
        use std::io::Write;
        use tower::ServiceExt;

        let mut encoder = flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        );
        encoder
            .write_all(b"precompressed source")
            .expect("gzip input");
        let encoded = Bytes::from(encoder.finish().expect("gzip output"));
        let expected = encoded.clone();
        let app = axum::Router::new()
            .route(
                "/encoded",
                axum::routing::get(move || {
                    let encoded = encoded.clone();
                    async move {
                        (
                            [(axum::http::header::CONTENT_ENCODING, "gzip")],
                            encoded,
                        )
                    }
                }),
            )
            .layer(axum::middleware::from_fn(mark_precompressed))
            .layer(tower_http::compression::CompressionLayer::new().gzip(true))
            .layer(axum::middleware::from_fn(offload_compression));
        let _busy = COMPRESSION.acquire().await.expect("compression slot");
        let bytes =
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                let response = app
                    .oneshot(
                        Request::get("/encoded")
                            .header("accept-encoding", "gzip")
                            .body(Body::empty())
                            .expect("request"),
                    )
                    .await
                    .expect("response");
                assert_eq!(
                    response
                        .headers()
                        .get(axum::http::header::CONTENT_ENCODING)
                        .expect("encoding"),
                    "gzip"
                );
                axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("encoded body")
            })
            .await
            .expect(
                "precompressed I/O must not wait for compression admission",
            );
        assert_eq!(bytes, expected);
    }

    /// A caller that stops polling its body must not retain the shared compression slot.
    #[tokio::test]
    async fn pending_body_releases_compression_admission() {
        let (started, entered) = tokio::sync::oneshot::channel();
        let stream = futures_util::stream::once(async move {
            started.send(()).expect("body polled");
            std::future::pending::<Result<Bytes, std::io::Error>>().await
        });
        let mut body = CpuBody::from(Body::from_stream(stream));
        // Drive admission with the real task waker until the source is reached, then stop polling the body.
        let reached = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            futures_util::future::select(
                entered,
                Box::pin(futures_util::future::poll_fn(|cx| {
                    Pin::new(&mut body).poll_frame(cx)
                })),
            ),
        )
        .await
        .expect("poll scheduled");
        assert!(
            matches!(reached, futures_util::future::Either::Left((Ok(()), _))),
            "source must be polled without returning a frame"
        );
        let _available = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            COMPRESSION.acquire(),
        )
        .await
        .expect("pending client cannot retain compression capacity")
        .expect("compression admission");
    }

    /// An early self-wake exercises the race between source readiness and the CPU reply.
    struct Source {
        caller: std::thread::ThreadId,
        next: u8,
    }

    impl HttpBody for Source {
        type Data = Bytes;
        type Error = std::io::Error;

        /// Emits pending, data and trailers while rejecting polls on the async caller.
        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            assert_ne!(self.caller, std::thread::current().id());
            self.next += 1;
            match self.next {
                1 => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                2 => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
                    b"payload",
                ))))),
                3 => Poll::Ready(Some(Ok(Frame::trailers(
                    axum::http::HeaderMap::from_iter([(
                        axum::http::header::HeaderName::from_static(
                            "x-finished",
                        ),
                        "yes".parse().expect("header"),
                    )]),
                )))),
                _ => Poll::Ready(None),
            }
        }
    }

    /// CPU offloading must preserve readiness, bytes, trailers and stream completion.
    #[tokio::test]
    async fn polls_off_executor_and_preserves_early_wakes_and_trailers() {
        let mut body = CpuBody::from(Body::new(Source {
            caller: std::thread::current().id(),
            next: 0,
        }));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let data = futures_util::future::poll_fn(|cx| {
                Pin::new(&mut body).poll_frame(cx)
            })
            .await
            .expect("data")
            .expect("frame");
            assert_eq!(data.into_data().expect("data frame"), "payload");
            let trailers = futures_util::future::poll_fn(|cx| {
                Pin::new(&mut body).poll_frame(cx)
            })
            .await
            .expect("trailers")
            .expect("frame");
            assert_eq!(
                trailers
                    .into_trailers()
                    .expect("trailer frame")
                    .get("x-finished")
                    .expect("header"),
                "yes"
            );
            assert!(
                futures_util::future::poll_fn(
                    |cx| Pin::new(&mut body).poll_frame(cx)
                )
                .await
                .is_none()
            );
            assert!(body.is_end_stream());
        })
        .await
        .expect("stream must not lose its wakeup");
    }
}
