//! Aggregates the jobs endpoints while each leaf module owns one documented route.
mod events;
mod result;
mod status;
mod upload;

use crate::state::AppState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Shares one tag between endpoint macros and the top-level OpenAPI metadata.
pub(crate) const TAG: &str = "JOBS";

/// Registers documented handlers directly so leaf modules need no forwarding routers.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(upload::upload))
        .routes(routes!(status::status))
        .routes(routes!(events::events))
        .routes(routes!(result::result))
}
