//! Shared display strings. Russian text lives here on purpose: `pacode-tui`
//! forbids Cyrillic inside string literals (`no_cyrillic_tests`), so callers
//! compose lines from these pieces instead of writing the words inline.

use crate::RenderOptions;

#[cfg(test)]
#[path = "text_tests.rs"]
mod text_tests;

/// Pick the Russian plural form for `n`: `plural(1, "строка", "строки", "строк")`.
/// 11–14 always take `many`.
pub fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    if (11..=14).contains(&(n % 100)) {
        many
    } else {
        match n % 10 {
            1 => one,
            2..=4 => few,
            _ => many,
        }
    }
}

/// `строка`/`строки`/`строк` for `n`.
pub fn lines_noun(n: usize) -> &'static str {
    plural(n, "строка", "строки", "строк")
}

/// `… ещё N строк · ctrl+o развернуть` — the fold marker under a capped
/// tool-call body (spec §6).
pub fn more_lines(n: usize, opts: &RenderOptions) -> String {
    format!(
        "{} ещё {n} {}{}ctrl+o развернуть",
        opts.glyphs.ellipsis,
        lines_noun(n),
        opts.glyphs.dot_sep,
    )
}

/// The backgrounded outcome word (`◍ в фоне · 1m47s`).
pub fn in_background() -> &'static str {
    "в фоне"
}

/// Display label for a built-in tool (spec §6: `Команда`, `Чтение`, `Поиск`,
/// `Запись`). `None` for tools without one — the caller falls back to the raw
/// name, capitalized.
pub fn tool_label(name: &str) -> Option<&'static str> {
    Some(match name {
        "bash" => "Команда",
        "read" | "read_file" => "Чтение",
        "grep" | "glob" | "search" => "Поиск",
        "ls" => "Список",
        "write" | "write_file" => "Запись",
        "edit" | "multi_edit" | "edit_file" | "apply_patch" => "Правка",
        "agent" => "Агент",
        _ => return None,
    })
}
