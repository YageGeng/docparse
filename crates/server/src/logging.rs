//! Log-event filtering keeps the lightweight correlation spans available at every event level.
use tracing_subscriber::EnvFilter;

/// Reserved for correlation spans; ordinary events retain their module targets and RUST_LOG levels.
pub(crate) const CONTEXT_TARGET: &str = "docparse::context";

/// Keeps correlation metadata while applying the caller's filter to ordinary log events.
pub fn filter(environment: EnvFilter) -> EnvFilter {
    // A target directive enables only these spans; a span-name directive would also enable their child events.
    environment.add_directive(
        format!("{CONTEXT_TARGET}=info").parse().expect(
            "the static correlation target is a valid tracing directive",
        ),
    )
}
