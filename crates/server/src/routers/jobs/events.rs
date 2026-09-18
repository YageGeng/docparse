use crate::{
    code::ApiCode,
    error::{ApiError, ApiResult, DatabaseSnafu, RequestSnafu, SerializeSnafu},
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
use std::{
    collections::HashMap, convert::Infallible, sync::Arc, time::Duration,
};
use tokio::sync::{Mutex, watch};
use tracing::{Instrument, instrument::WithSubscriber};
use uuid::Uuid;

/// One replaceable snapshot is shared by all subscribers to a job on this API instance.
#[derive(Clone)]
pub(crate) struct JobEvent {
    version: i64,
    event: Event,
    terminal: bool,
}

/// Owns only active pollers; dropping the last receiver releases its task and registry entry.
#[derive(Default)]
pub struct Subscriptions {
    channels: Mutex<HashMap<Uuid, watch::Sender<JobEvent>>>,
}

impl TryFrom<JobSnapshot> for JobEvent {
    type Error = ApiError;

    /// Serializes once per revision rather than once per connected browser.
    fn try_from(job: JobSnapshot) -> ApiResult<Self> {
        Ok(Self {
            version: job.version,
            terminal: job.is_terminal(),
            event: Event::default()
                .event("job")
                .id(job.version.to_string())
                .data(serde_json::to_string(&ApiResponse::data(job)).context(
                    SerializeSnafu {
                        stage: "job-events-encode",
                        code: ApiCode::COMMON_INTERNAL_ERROR,
                    },
                )?),
        })
    }
}

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
    let first = JobEvent::try_from(JobSnapshot::from(first))?;
    let registry = Arc::clone(&state.subscriptions);
    let mut subscriptions = registry.channels.lock().await;
    let receiver = if let Some(sender) = subscriptions.get(&id) {
        // A fresh database read may be ahead of the shared poller; never replay an older revision.
        sender.send_if_modified(|current| {
            if first.version > current.version {
                *current = first.clone();
                true
            } else {
                false
            }
        });
        sender.subscribe()
    } else {
        let (sender, receiver) = watch::channel(first.clone());
        if !first.terminal {
            subscriptions.insert(id, sender.clone());
            let registry = Arc::clone(&registry);
            let poller = async move {
                tracing::debug!("started shared SSE polling for job {}", id);
                loop {
                    tokio::select! {
                        _ = state.shutdown.cancelled() => break,
                        _ = sender.closed() => break,
                        _ = tokio::time::sleep(state.options.poll_interval) => {}
                    }
                    let loaded = tokio::select! {
                        _ = state.shutdown.cancelled() => break,
                        _ = sender.closed() => break,
                        loaded = Jobs::find_by_id(&state.db, id) => loaded,
                    }
                    .with_context(|source| DatabaseSnafu {
                        stage: "job-events-poll",
                        code: ApiCode::from(&*source),
                    })
                    .and_then(|job| {
                        job.context(RequestSnafu {
                            stage: "job-events-poll",
                            code: ApiCode::not_found(4041001),
                        })
                    })
                    .and_then(|job| JobEvent::try_from(JobSnapshot::from(job)));
                    match loaded {
                        Ok(event) => {
                            let terminal = event.terminal;
                            sender.send_if_modified(|current| {
                                if event.version > current.version {
                                    *current = event.clone();
                                    true
                                } else {
                                    false
                                }
                            });
                            if terminal {
                                break;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                "SSE subscription failed for job {}: {}",
                                id,
                                error
                            );
                            match Event::default()
                                .event("error")
                                .json_data(ApiErrorResponse::from(error))
                            {
                                Ok(event) => {
                                    sender.send_replace(JobEvent {
                                        version: -1,
                                        event,
                                        terminal: true,
                                    });
                                }
                                Err(error) => tracing::error!(
                                    "failed to serialize SSE error for job {}: {}",
                                    id,
                                    error
                                ),
                            }
                            break;
                        }
                    }
                }
                let mut subscriptions = registry.channels.lock().await;
                if subscriptions
                    .get(&id)
                    .is_some_and(|current| current.same_channel(&sender))
                {
                    subscriptions.remove(&id);
                }
                tracing::debug!("stopped shared SSE polling for job {}", id);
            };
            tokio::spawn(poller.in_current_span().with_current_subscriber());
        }
        receiver
    };
    drop(subscriptions);
    // Each connection still replays immediately and closes only after delivering its terminal snapshot.
    let events = stream::unfold(Some((receiver, true)), |cursor| async move {
        let (mut receiver, first) = cursor?;
        if !first && receiver.changed().await.is_err() {
            return None;
        }
        let snapshot = receiver.borrow_and_update().clone();
        Some((
            Ok::<_, Infallible>(snapshot.event),
            (!snapshot.terminal).then_some((receiver, false)),
        ))
    });
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
