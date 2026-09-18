use crate::{
    code::ApiCode,
    error::{PanicHandler, RequestSnafu},
    middlewares::trace,
    routers::{common, docs, jobs},
    state::AppState,
};
use axum::{Extension, Router, extract::DefaultBodyLimit, middleware};
use docparse_config::{ConfigError, ServerConfig};
use std::sync::Arc;
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{
        CompressionLayer, CompressionLevel,
        predicate::{DefaultPredicate, NotForContentType, Predicate},
    },
};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

/// Service metadata complements the paths and schemas collected directly from handler macros.
#[derive(OpenApi)]
#[openapi(
    info(title = "DocParse API", description = "Durable PDF parsing jobs with JSON results and reconnectable SSE progress. Authentication and browser CORS policy are supplied by the deployment ingress."),
    tags(
        (name = jobs::TAG, description = "Submit PDFs, inspect progress, and retrieve durable results"),
        (name = common::TAG, description = "Process and shared-dependency health"),
        (name = docs::TAG, description = "Generated API reference")
    )
)]
struct ApiDoc;

/// Applies one validated API prefix to live routes and their generated documentation, preserving streaming middleware.
pub fn router(
    state: AppState,
    config: &ServerConfig,
) -> Result<Router, ConfigError> {
    // Validate the prefix and other configuration before routing; errors are fatal at startup.
    config.validate()?;

    let routes = OpenApiRouter::new()
        .merge(jobs::router())
        .merge(common::router())
        .merge(docs::router());
    let root = OpenApiRouter::with_openapi(ApiDoc::openapi());

    // Utoipa's nesting updates the live route tree and OpenAPI paths together; Axum rejects nesting at the root.
    let router = match config.api_prefix.as_str() {
        "" | "/" => root.merge(routes),
        prefix => root.nest(prefix, routes),
    };
    let (router, document) = router.split_for_parts();

    Ok(router
        // Routing failures produce typed errors before serialization, preserving Axum's Allow header.
        .fallback(|| async {
            RequestSnafu {
                stage: "http-route-find",
                code: ApiCode::COMMON_NOT_FOUND,
            }
            .build()
        })
        .method_not_allowed_fallback(|| async {
            RequestSnafu {
                stage: "http-route-method",
                code: ApiCode::method_not_allowed(405000),
            }
            .build()
        })
        .layer(Extension(Arc::new(document)))
        // Multipart framing receives a small separate allowance; PDF bytes have their own exact limit.
        .layer(DefaultBodyLimit::max(
            state.options.max_upload_bytes + 64 * 1024,
        ))
        .layer(CatchPanicLayer::custom(PanicHandler))
        // Compress incrementally; PDF byte ranges and event delivery must retain their original representation.
        .layer(
            CompressionLayer::new()
                .gzip(true)
                .quality(CompressionLevel::Fastest)
                .compress_when(
                    DefaultPredicate::new()
                        .and(NotForContentType::new("text/event-stream"))
                        .and(NotForContentType::new("application/pdf")),
                ),
        )
        .layer(middleware::from_fn(trace::request_trace))
        .with_state(state))
}
