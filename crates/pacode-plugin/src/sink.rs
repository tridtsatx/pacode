//! Host-provided UI sink interface for plugins.

pub trait UiSink: Send + Sync {
    fn toast(&self, text: &str);
    fn status(&self, text: &str);
}

/// A no-op implementation of [`UiSink`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopUiSink;

impl UiSink for NoopUiSink {
    fn toast(&self, _text: &str) {}
    fn status(&self, _text: &str) {}
}
