//! The render context the draw path reads: theme, colour mode, glyphs, the
//! render options handed to `pacode-render`, and the runtime-config hook that
//! rebuilds them.

use pacode_render::{ColorMode, Glyphs, RenderOptions};
use pacode_types::config::ShimmerMode;

use super::AppState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderContext {
    pub theme: pacode_render::Theme,
    pub glyphs: Glyphs,
    pub color: ColorMode,
    pub thinking: bool,
    pub expand_hint: String,
}

pub(super) fn expand_hint_for(keymap: &crate::binding::Keymap) -> String {
    keymap
        .bindings_for(crate::binding::Action::ExpandOutput)
        .first()
        .map(crate::binding::format_binding)
        .unwrap_or_else(|| "click".to_string())
}

impl AppState {
    pub fn render_options(&self, width: u16) -> RenderOptions {
        RenderOptions::with_theme_color(
            width,
            self.config.ui.ascii_only,
            self.theme.clone(),
            self.color_mode,
        )
        .thinking(self.config.ui.thinking)
        .expand_hint(expand_hint_for(&self.keymap))
        .cache_epoch(self.render_epoch)
        .shimmer(self.shimmer_mode())
        .anim(self.anim)
    }

    /// The sweep to draw, if any.
    ///
    /// A terminal with no colour cannot show a sweep, and a monochrome client
    /// should not pay for the per-character walk that produces one: the tiers
    /// collapse to a single style, so the animation would be invisible work on
    /// every frame.
    pub fn shimmer_mode(&self) -> ShimmerMode {
        if self.color_mode == ColorMode::Mono {
            ShimmerMode::Off
        } else {
            self.config.ui.shimmer
        }
    }

    pub fn sync_render_context(&mut self) {
        let next = RenderContext {
            theme: self.theme.clone(),
            glyphs: Glyphs::new(self.config.ui.ascii_only),
            color: self.color_mode,
            thinking: self.config.ui.thinking,
            expand_hint: expand_hint_for(&self.keymap),
        };
        if self.render_context == next {
            return;
        }
        self.render_context = next;
        self.render_epoch = self.render_epoch.wrapping_add(1);
        let truecolor = self.truecolor;
        if let Some(header) = &mut self.transcript.header {
            header.truecolor = truecolor;
        }
        self.transcript.cache.clear();
        self.panel.agent_transcript.cache.clear();
        if let Some(diff) = &mut self.panel.diff {
            diff.rendered = None;
        }
        if let Some(diff) = &mut self.git.diff {
            diff.rendered = None;
        }
    }

    pub fn rebuild_theme(&mut self) {
        self.color_mode = ColorMode::from_setting(&self.config.ui.color, self.detected_color_mode);
        self.truecolor = self.color_mode == ColorMode::Truecolor;
        let (palette, _) = pacode_config::theme::load_theme(&self.paths, &self.config.theme);
        self.theme = pacode_render::Theme::from_palette(&palette, self.truecolor);
    }

    pub fn apply_runtime_config(&mut self, key: &str) {
        if key == "ui.color" || key.starts_with("theme.") {
            self.rebuild_theme();
        }
        if key == "ui.transcript_cells" {
            let max_cells = self.config.ui.transcript_cells;
            self.transcript.set_max_cells(max_cells);
            self.panel.agent_transcript.set_max_cells(max_cells);
        }
        self.sync_render_context();
    }
}
