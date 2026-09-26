//! Tracing setup for the UI app.
//!
//! Two things live here:
//! - `init_tracing`: the global subscriber (filter + fmt layer).
//! - `QuietClipboardPaste`: a `FormatEvent` wrapper that silently drops
//!   egui-winit's "arboard paste error" line — expected noise, since
//!   egui-winit only supports pasting TEXT, so every Ctrl/Cmd+V with an
//!   image-only clipboard logs it even though our own image-paste handler
//!   (ui/input.rs) reads the clipboard fine. All other events are delegated
//!   to the default formatter unchanged.
//!
//!   (A `filter_fn` can't do this: it only sees event *metadata*, not the
//!   message body; the clipboard target also logs genuinely useful errors
//!   like "arboard copy/cut error" and "Failed to initialize arboard
//!   clipboard".)

use tracing_subscriber::prelude::*;

type DefaultFmt = tracing_subscriber::fmt::format::Format<
    tracing_subscriber::fmt::format::Full,
    tracing_subscriber::fmt::time::SystemTime,
>;

struct QuietClipboardPaste(DefaultFmt);

/// Captures the value of the `message` field of an event (used to tell the
/// noisy "arboard paste error" line apart from the genuinely useful
/// copy/cut errors logged from the same target).
struct MessageCapture(Option<String>);

impl tracing::field::Visit for MessageCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = Some(value.to_string());
        }
    }
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for QuietClipboardPaste
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let is_expected_paste_noise = {
            if event.metadata().target() != "egui_winit::clipboard" {
                false
            } else {
                let mut capture = MessageCapture(None);
                event.record(&mut capture);
                capture
                    .0
                    .as_deref()
                    .is_some_and(|m| m.starts_with("arboard paste error"))
            }
        };
        if is_expected_paste_noise {
            return Ok(());
        }
        self.0.format_event(ctx, writer, event)
    }
}

/// Initialize the global tracing subscriber (call ONCE, at startup).
///
/// Default to `debug` for app crates, but silence the extremely chatty
/// `naga` WGSL shader compiler (pulled in by wgpu/egui) whose DEBUG-level
/// overload-resolution traces flood the console at startup. Users can still
/// override the whole filter via RUST_LOG.
///
/// The fmt layer additionally drops egui-winit's "arboard paste error"
/// line: it is expected noise (see `QuietClipboardPaste`).
pub fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug,naga=off")),
        )
        .with(
            tracing_subscriber::fmt::layer().event_format(QuietClipboardPaste(Default::default())),
        )
        .init();
}
