//! Classifies the successful responses of one route subtree as routine traffic.

use axum::{
    extract::Request, http::StatusCode, middleware::Next, response::Response,
};

/// Marks a completion the trace layer may report at `DEBUG`, such as serving one asset.
///
/// The name describes the logged event rather than the response, because the same
/// subtree also produces failures that must stay visible. [`mark_routine`] inserts
/// the marker, so a route subtree declares the policy instead of every handler
/// repeating it.
#[derive(Clone, Copy, Debug)]
pub struct RoutineCompletion;

/// Marks every successful response below the layer with [`RoutineCompletion`].
///
/// Apply it to a subtree with `axum::middleware::from_fn(middlewares::mark_routine)`,
/// the way the workbench mount does. Only served and revalidated responses are
/// marked: a missing asset, a rejected method or an API response keeps its `INFO`
/// line so failures stay visible.
pub async fn mark_routine(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if response.status().is_success()
        || response.status() == StatusCode::NOT_MODIFIED
    {
        response.extensions_mut().insert(RoutineCompletion);
    }
    response
}
