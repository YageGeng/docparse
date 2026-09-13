use crate::{
    code::ApiCode,
    error::{ApiResult, DatabaseSnafu, RequestSnafu},
    model::{
        base::ApiResponse,
        error::ApiErrorResponse,
        job::{JobQuery, JobSnapshot},
    },
    state::AppState,
};
use axum::{
    extract::{Query, State, rejection::QueryRejection},
    http::header,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use docparse_database::query::parse_job::ParseJobQuery as Jobs;
use futures_util::{Stream, stream};
use snafu::{OptionExt, ResultExt};
use std::{convert::Infallible, time::Duration};
use tracing::{Instrument, instrument::WithSubscriber};

/// Replays the latest durable snapshot on every connection and polls revisions without owning the parse task.
#[utoipa::path(
    get, path = "/jobs/events", tag = super::TAG,
    description = "SSE event `job` contains ApiResponse<JobSnapshot> JSON; its id is the persisted version. Every connection replays the latest snapshot, even with Last-Event-ID. Intermediate progress is coalesced rather than retained as event history. Terminal snapshots close the stream: clients should close EventSource on succeeded/failed. Disconnects do not cancel parsing. Heartbeats are sent every 15 seconds; database failure after HTTP 200 is sent as an `error` event containing ApiErrorResponse, then the stream closes.",
    params(
        JobQuery,
        ("Last-Event-ID" = Option<String>, Header, description = "May be supplied by EventSource; the latest snapshot is always replayed")
    ),
    responses(
        (status = 200, description = "SSE frames carrying task snapshots or errors", body = String, content_type = "text/event-stream",
            headers(("X-Accel-Buffering" = String, description = "no"), ("Cache-Control" = String, description = "no-cache, no-transform")),
            example = "event: job\nid: 4\ndata: {\"data\":{\"id\":\"07bd9078-a15f-4b41-bbca-e341047db61e\",\"status\":\"succeeded\",\"version\":4,\"attempts\":1,\"progress\":{\"stage\":\"complete\",\"total\":3},\"error\":null},\"success\":true,\"message\":\"Success\"}\n\n"),
        (status = 400, description = "Invalid task UUID (4001002)", body = ApiErrorResponse),
        (status = 404, description = "Task not found (4041001)", body = ApiErrorResponse),
        (status = 503, description = "Task database unavailable before streaming starts (5031002)", body = ApiErrorResponse),
        (status = 500, description = "Internal error before streaming starts (500000)", body = ApiErrorResponse)
    )
)]
pub async fn events(
    State(state): State<AppState>,
    query: Result<Query<JobQuery>, QueryRejection>,
) -> ApiResult<Response> {
    // Query extraction keeps task identifiers out of route templates and works with native EventSource.
    let Query(JobQuery { id }) = query.map_err(|_rejection| {
        RequestSnafu {
            stage: "job-events-parse-id",
            code: ApiCode::bad_request(4001002),
        }
        .build()
    })?;
    tracing::Span::current().record("job_id", tracing::field::display(id));
    let first = Jobs::find_by_id(&state.db, id)
        .await
        .with_context(|source| DatabaseSnafu {
            stage: "task-subscribe-events",
            code: ApiCode::from(&*source),
        })?
        .context(RequestSnafu {
            stage: "job-events-find",
            code: ApiCode::not_found(4041001),
        })?;
    tracing::Span::current()
        .record("pdf_hash", tracing::field::display(&first.input_hash));
    // SSE polling outlives the handler, so capture its subscriber here rather than from the later body-polling task.
    let span = tracing::Span::current();
    let dispatcher = tracing::dispatcher::get_default(Clone::clone);
    // Snapshot replay intentionally coalesces intermediate events; terminal state never depends on a transient channel.
    let events = stream::unfold(
        Some((state, Some(first), -1_i64)),
        move |cursor| {
            let span = span.clone();
            let dispatcher = dispatcher.clone();
            async move {
            let (state, mut pending, mut version) = cursor?;
            loop {
                let loaded = match pending.take() {
                    Some(job) => Ok(Some(job)),
                    None => {
                        tokio::select! {
                            _ = state.shutdown.cancelled() => return None,
                            _ = tokio::time::sleep(state.options.poll_interval) => {}
                        }
                        Jobs::find_by_id(&state.db, id).await
                    }
                };
                // Preserve stage and source for SSE failures using the same ErrorCode conversion as ordinary HTTP.
                let loaded = loaded
                    .with_context(|source| DatabaseSnafu {
                        stage: "job-events-poll",
                        code: ApiCode::from(&*source),
                    })
                    .and_then(|job| {
                        job.context(RequestSnafu {
                            stage: "job-events-poll",
                            code: ApiCode::not_found(4041001),
                        })
                    });
                let job = match loaded {
                    Ok(job) => JobSnapshot::from(job),
                    Err(error) => {
                        tracing::warn!("SSE subscription failed: {}", error);
                        let event = Event::default()
                            .event("error")
                            .json_data(ApiErrorResponse::from(error))
                            .ok()?;
                        return Some((Ok::<_, Infallible>(event), None));
                    }
                };
                if job.version == version {
                    continue;
                }
                version = job.version;
                let terminal = job.is_terminal();
                let event = Event::default()
                    .event("job")
                    .id(version.to_string())
                    .json_data(ApiResponse::data(job))
                    .ok()?;
                return Some((
                    Ok::<_, Infallible>(event),
                    (!terminal).then_some((state, None, version)),
                ));
            }
            }.instrument(span).with_subscriber(dispatcher)
        },
    );
    Ok(sse_response(events))
}

/// Applies streaming proxy headers and keep-alives without buffering progress bodies.
fn sse_response(
    events: impl Stream<Item = Result<Event, Infallible>> + Send + 'static,
) -> Response {
    let mut response = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}
