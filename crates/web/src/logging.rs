//! Browser tracing and panic reporting, independent of the parser ABI.
use std::sync::atomic::{AtomicUsize, Ordering};

/// Installs Worker-local diagnostics before initializing third-party runtimes.
pub(crate) fn init() {
    let _ = tracing::subscriber::set_global_default(BrowserLog);
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(
            &format!("DocParse WASM panic: {info}").into(),
        )
    }));
}

/// Minimal subscriber that records meaningful event messages in the browser console.
struct BrowserLog;
impl tracing::Subscriber for BrowserLog {
    /// Limits release browser logging to lifecycle and error events.
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }
    /// Allocates a unique correlation identifier without browser-owned state.
    fn new_span(
        &self,
        _attributes: &tracing::span::Attributes<'_>,
    ) -> tracing::span::Id {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        tracing::span::Id::from_u64(NEXT.fetch_add(1, Ordering::Relaxed) as u64)
    }
    /// Leaves span fields unrecorded because event messages already contain diagnostic context.
    fn record(
        &self,
        _span: &tracing::span::Id,
        _values: &tracing::span::Record<'_>,
    ) {
    }
    /// Does not retain causal links between completed spans.
    fn record_follows_from(
        &self,
        _span: &tracing::span::Id,
        _follows: &tracing::span::Id,
    ) {
    }
    /// Emits the formatted message without dumping structured payloads.
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        if *event.metadata().level() == tracing::Level::ERROR {
            web_sys::console::error_1(&message.0.into());
        } else {
            web_sys::console::log_1(&message.0.into());
        }
    }
    /// Does not retain Worker-local span entry state.
    fn enter(&self, _span: &tracing::span::Id) {}
    /// Does not retain Worker-local span exit state.
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Captures only tracing's readable event message.
struct Message(String);
impl tracing::field::Visit for Message {
    /// Ignores non-message fields to avoid logging arbitrary payloads.
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn std::fmt::Debug,
    ) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}
