//! The glyph table with its ASCII fallback. Colour is never the only carrier
//! of meaning: every status has a glyph too (spec §13).

/// Glyphs with ASCII fallback (`ui.ascii_only`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyphs {
    pub ascii: bool,
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    pub running: &'static str,
    pub pointer: &'static str,
    pub ok: &'static str,
    pub fail: &'static str,
    pub main_dot: &'static str,
    pub agent_dot: &'static str,
    pub tool: &'static str,
    pub prompt: &'static str,
    pub check_done: &'static str,
    pub check_active: &'static str,
    pub check_pending: &'static str,
    pub arrow_down: &'static str,
    pub arrow_up: &'static str,
    pub arrow_right: &'static str,
    pub dot_sep: &'static str,
    pub ellipsis: &'static str,
    pub vline: &'static str,
    /// The `└` ending a tool cell's body — the gutter's last elbow.
    pub vcorner: &'static str,
    pub hline: &'static str,
    pub chevrons: &'static str,
    pub disabled: &'static str,
    pub stopped: &'static str,
}

impl Glyphs {
    pub fn new(ascii_only: bool) -> Self {
        if ascii_only {
            Self {
                ascii: true,
                bar_full: "#",
                bar_empty: "-",
                running: "*",
                pointer: ">",
                ok: "+",
                fail: "x",
                main_dot: "@",
                agent_dot: "o",
                tool: "#",
                prompt: ">",
                check_done: "[x]",
                check_active: "[*]",
                check_pending: "[ ]",
                arrow_down: "v",
                arrow_up: "^",
                arrow_right: "->",
                dot_sep: " . ",
                ellipsis: "...",
                vline: "|",
                vcorner: "`",
                hline: "-",
                chevrons: ">>",
                disabled: "o",
                stopped: "-",
            }
        } else {
            Self {
                ascii: false,
                bar_full: "▰",
                bar_empty: "▱",
                running: "◍",
                pointer: "▸",
                ok: "✓",
                fail: "✗",
                main_dot: "●",
                agent_dot: "○",
                tool: "▣",
                prompt: "❯",
                check_done: "[✓]",
                check_active: "[•]",
                check_pending: "[ ]",
                arrow_down: "↓",
                arrow_up: "↑",
                arrow_right: "→",
                dot_sep: " · ",
                ellipsis: "…",
                vline: "│",
                vcorner: "└",
                hline: "─",
                chevrons: "»",
                disabled: "⊘",
                stopped: "■",
            }
        }
    }

    /// Braille spinner frame for `frame` (`-\|/` under ASCII). A method rather
    /// than a field because the glyph is a cycle, not a constant.
    pub fn spinner(&self, frame: u64) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        const ASCII: [&str; 4] = ["-", "\\", "|", "/"];
        if self.ascii {
            ASCII[(frame as usize) % ASCII.len()]
        } else {
            FRAMES[(frame as usize) % FRAMES.len()]
        }
    }

    /// `▰▰▰▰▰▱▱▱` for `percent` over `width` cells.
    pub fn progress_bar(&self, percent: u8, width: usize) -> String {
        let width = width.max(1);
        let filled = ((percent.min(100) as usize) * width)
            .div_ceil(100)
            .min(width);
        let mut out = String::with_capacity(width * 3);
        for _ in 0..filled {
            out.push_str(self.bar_full);
        }
        for _ in filled..width {
            out.push_str(self.bar_empty);
        }
        out
    }
}
