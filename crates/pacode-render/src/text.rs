//! Shared display strings for the transcript and menus.

use crate::RenderOptions;

#[cfg(test)]
#[path = "text_tests.rs"]
mod text_tests;

/// `line`/`lines` for `n`.
pub fn lines_noun(n: usize) -> &'static str {
    if n == 1 { "line" } else { "lines" }
}

/// `… 12 more lines · ctrl+o to expand` — the fold marker under a capped
/// tool-call body (spec §6). `opts.expand_hint` names the bound key.
pub fn more_lines(n: usize, opts: &RenderOptions) -> String {
    format!(
        "{} {n} more {}{}{} to expand",
        opts.glyphs.ellipsis,
        lines_noun(n),
        opts.glyphs.dot_sep,
        opts.expand_hint,
    )
}

/// The backgrounded outcome word (`◍ background · 1m47s`).
pub fn in_background() -> &'static str {
    "background"
}

/// The failed-count word in a hub summary (`4 · 1 failed`).
pub fn failed_word(_n: usize) -> &'static str {
    "failed"
}

/// Display label for a built-in tool (spec §6: `Bash`, `Read`, `Search`,
/// `Write`). `None` for tools without one — the caller falls back to the raw
/// name, capitalized.
pub fn tool_label(name: &str) -> Option<&'static str> {
    Some(match name {
        "bash" => "Bash",
        "read" | "read_file" => "Read",
        "grep" | "glob" | "search" => "Search",
        "ls" => "List",
        "write" | "write_file" => "Write",
        "edit" | "multi_edit" | "edit_file" | "apply_patch" => "Edit",
        "agent" => "Agent",
        _ => return None,
    })
}
