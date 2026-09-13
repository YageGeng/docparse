//! Aggregates the docs endpoints while each leaf module owns one documented route.
mod openapi;
mod scalar;

use crate::state::AppState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Shares one tag between endpoint macros and the top-level OpenAPI metadata.
pub(crate) const TAG: &str = "Documentation";

/// Registers documented handlers directly so leaf modules need no forwarding routers.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(openapi::openapi))
        .routes(routes!(scalar::scalar))
}
