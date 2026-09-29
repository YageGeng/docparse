use crate::{
    code::ApiCode,
    error::{ApiError, PanicHandler, RequestSnafu},
    middlewares::trace,
    routers::{common, docs, jobs},
    state::AppState,
    workbench,
};
use axum::{
    Extension, Router,
    body::Body,
    extract::DefaultBodyLimit,
    http::{Request, Uri},
    middleware,
    response::Redirect,
};
use docparse_config::{ConfigError, ServerConfig};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use tower::{ServiceBuilder, service_fn};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{
        CompressionLayer, CompressionLevel,
        predicate::{DefaultPredicate, NotForContentType, Predicate},
    },
};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

/// Announces the workbench URL once the listening address is known.
///
/// Operators open this page directly, so the log carries a full clickable URL instead of only
/// the mount path; a wildcard bind is reported as loopback because `0.0.0.0` is not clickable.
pub fn announce_workbench(bound: SocketAddr, config: &ServerConfig) {
    if config.webui.is_none() {
        return;
    }
    tracing::info!(
        "workbench available at http://{}{}",
        clickable_address(bound),
        workbench::mount_path(&config.api_prefix)
    );
}

/// Maps a wildcard bind address to the loopback address a browser can open.
fn clickable_address(bound: SocketAddr) -> SocketAddr {
    if !bound.ip().is_unspecified() {
        return bound;
    }
    match bound {
        SocketAddr::V4(_) => {
            SocketAddr::from((Ipv4Addr::LOCALHOST, bound.port()))
        }
        SocketAddr::V6(_) => {
            SocketAddr::from((Ipv6Addr::LOCALHOST, bound.port()))
        }
    }
}

/// Service metadata complements the paths and schemas collected directly from handler macros.
#[derive(OpenApi)]
#[openapi(
    info(title = "DocParse API", description = "Durable PDF parsing jobs with JSON results and reconnectable SSE progress. Authentication and browser CORS policy are supplied by the deployment ingress."),
    components(schemas(crate::model::monitoring::Chart)),
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
        .merge(crate::routers::monitoring::router())
        .merge(docs::router());
    let root = OpenApiRouter::with_openapi(ApiDoc::openapi());

    // Utoipa's nesting updates the live route tree and OpenAPI paths together; Axum rejects nesting at the root.
    let router = match config.api_prefix.as_str() {
        "" | "/" => root.merge(routes),
        prefix => root.nest(prefix, routes),
    };
    let (router, document) = router.split_for_parts();

    let mut router = router
        .route(
            "/metrics",
            axum::routing::get(crate::routers::monitoring::metrics),
        )
        // Unmatched routes keep the typed JSON envelope. The workbench is nested
        // below the API prefix, so no API path can ever reach the application shell.
        .fallback(|| async { ApiError::route_not_found() })
        // Routing failures produce typed errors before serialization, preserving Axum's Allow header.
        .method_not_allowed_fallback(|| async {
            RequestSnafu {
                stage: "http-route-method",
                code: ApiCode::method_not_allowed(405000),
            }
            .build()
        });

    // The workbench is mounted at `{api_prefix}/webui`, and the root redirects there
    // so the UI stays discoverable without claiming the root namespace.
    if let Some(workbench) = workbench::Workbench::resolve(config)? {
        let mount = workbench.mount().to_owned();
        let workbench = Arc::new(workbench);
        let index = format!("{mount}/");
        router = router
            .route(
                "/",
                axum::routing::get(move |uri: Uri| {
                    let index = index.clone();
                    async move {
                        // A bookmarked deep link keeps its query through the hop.
                        let target = match uri.query() {
                            Some(query) => format!("{index}?{query}"),
                            None => index,
                        };
                        Redirect::temporary(&target)
                    }
                }),
            )
            .nest_service(
                &mount,
                ServiceBuilder::new()
                    // The workbench subtree is routine traffic, while API and failure
                    // responses keep their INFO lines.
                    .layer(middleware::from_fn(
                        crate::middlewares::mark_routine,
                    ))
                    .service(service_fn(move |request: Request<Body>| {
                        let workbench = Arc::clone(&workbench);
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                workbench.respond(request).await,
                            )
                        }
                    })),
            );
    }

    Ok(router
        .layer(Extension(Arc::new(document)))
        // Multipart framing receives a small separate allowance; PDF bytes have their own exact limit.
        .layer(DefaultBodyLimit::max(
            state.options.max_upload_bytes + 64 * 1024,
        ))
        .layer(CatchPanicLayer::custom(PanicHandler))
        .layer(middleware::from_fn(
            crate::middlewares::cpu_body::mark_precompressed,
        ))
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
        .layer(middleware::from_fn(
            crate::middlewares::cpu_body::offload_compression,
        ))
        .layer(middleware::from_fn(trace::request_trace))
        .with_state(state))
}

#[cfg(test)]
mod tests {
    use super::clickable_address;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    /// Wildcard binds become loopback URLs, and explicit hosts stay untouched.
    #[test]
    fn clickable_addresses_are_openable() {
        assert_eq!(
            clickable_address(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080))),
            SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))
        );
        assert_eq!(
            clickable_address(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 8080))),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 8080))
        );
        let explicit =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)), 9000);
        assert_eq!(clickable_address(explicit), explicit);
    }
}
