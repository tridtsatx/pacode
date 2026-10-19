//! Diffs for the edit tools and the transcript: `<number> <sign> <code>` rows
//! with a full-width background band on added/removed lines, syntax-coloured
//! code, and context folded to three lines around each change. Degrades by
//! `RenderOptions::color`: `Ansi` drops the band, `Mono` drops the colours.

use std::time::Duration;

use pacode_types::DiffStat;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::RenderOptions;
use crate::syntax::{Lexer, Token, token_style};
use crate::theme::ColorMode;
use crate::wrap::{display_width, truncate_spans};

#[cfg(test)]
#[path = "diff_tests.rs"]
mod diff_tests;

/// Wall-clock budget for the line diff. Myers is O((N+M)·D): rewriting most
/// of a multi-megabyte file would otherwise run for minutes. Past the budget
/// `similar` stops refining and emits a coarser (still exact) delete+insert
/// diff, so the text and the counts stay true — only less minimal.
pub const DIFF_TIMEOUT: Duration = Duration::from_millis(500);

/// Unified diff text with `context` lines and `a/<path>` / `b/<path>` headers,
/// plus its added/removed line counts — both from one diff pass, bounded by
/// [`DIFF_TIMEOUT`].
pub fn unified_diff(old: &str, new: &str, path: &str, context: usize) -> (String, DiffStat) {
    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(old, new);
    let mut stat = DiffStat {
        added: 0,
        removed: 0,
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => stat.added += 1,
            ChangeTag::Delete => stat.removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let a_header = format!("a/{path}");
    let b_header = format!("b/{path}");
    let text = diff
        .unified_diff()
        .context_radius(context)
        .header(&a_header, &b_header)
        .to_string();
    (text, stat)
}

/// Context lines kept on each side of a change; longer runs fold into
/// `… N lines collapsed · ctrl+o to expand`.
const CONTEXT: usize = 3;

/// `… N lines collapsed · <key> to expand` — the same marker idiom the tool
/// cell uses for its `more lines` fold.
fn folded_text(n: usize, opts: &RenderOptions) -> String {
    let noun = crate::text::lines_noun(n);
    format!(
        "{} {n} {noun} collapsed{}{} to expand",
        opts.glyphs.ellipsis, opts.glyphs.dot_sep, opts.expand_hint,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    /// `--- a/x`, `+++ b/x`, `diff --git`, `index`, mode lines — faint.
    Header,
    /// `@@ -a,b +c,d @@` — dim.
    Hunk,
    /// `\ No newline at end of file` — faint, no number.
    Meta,
    /// `… N lines collapsed · ctrl+o to expand` placeholder.
    Fold(usize),
    Add,
    Del,
    Ctx,
}

struct Row {
    kind: RowKind,
    /// Line text without the leading `+`/`-`/` ` marker for code rows,
    /// the raw line otherwise.
    text: String,
    /// Displayed line number: new file for `+`/context, old file for `-`.
    num: usize,
    /// Lexed tokens of `text` (byte ranges into it); `None` without a lexer.
    toks: Option<Vec<Token>>,
}

fn is_header_line(raw: &str) -> bool {
    raw.starts_with("diff ")
        || raw.starts_with("index ")
        || raw.starts_with("--- ")
        || raw.starts_with("+++ ")
        || raw.starts_with("new file")
        || raw.starts_with("deleted file")
        || raw.starts_with("old mode")
        || raw.starts_with("new mode")
        || raw.starts_with("similarity ")
        || raw.starts_with("dissimilarity ")
        || raw.starts_with("rename ")
        || raw.starts_with("copy ")
        || raw.starts_with("Binary ")
        || raw.starts_with("GIT binary patch")
}

/// `--- a/x` / `+++ b/x` → `x`; `a/` `b/` prefixes and `\ No newline`-style
/// trailer fields stripped. `/dev/null` carries no language.
fn header_path(raw: &str) -> Option<&str> {
    let p = raw
        .strip_prefix("--- ")
        .or_else(|| raw.strip_prefix("+++ "))?;
    let p = p.split('\t').next().unwrap_or(p).trim();
    if p == "/dev/null" || p.is_empty() {
        return None;
    }
    Some(
        p.strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(p),
    )
}

/// `@@ -old[,n] +new[,n] @@ …` → (old_start, old_count, new_start, new_count).
/// The counts matter: a `--- x` inside a live hunk is a removed line, not a
/// header, and only the counters tell the two apart.
fn hunk_starts(raw: &str) -> Option<(usize, usize, usize, usize)> {
    fn range(spec: &str) -> Option<(usize, usize)> {
        let mut it = spec.split(',');
        let start = it.next()?.parse().ok()?;
        let count = it.next().and_then(|c| c.parse().ok()).unwrap_or(1);
        Some((start, count))
    }
    let mut old = None;
    let mut new = None;
    for tok in raw.split_whitespace() {
        if let Some(spec) = tok.strip_prefix('-') {
            old = old.or_else(|| range(spec));
        } else if let Some(spec) = tok.strip_prefix('+') {
            new = new.or_else(|| range(spec));
        }
    }
    match (old, new) {
        (Some((os, oc)), Some((ns, nc))) => Some((os, oc, ns, nc)),
        (Some(_), None) | (None, Some(_)) | (None, None) => None,
    }
}

/// Split the diff into rows, numbering lines per hunk and lexing code rows
/// with the file's language while the lexer is still in order — folded rows
/// must already have contributed their state before they disappear.
fn parse_rows(diff: &str, want_lex: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut lexer: Option<Lexer> = None;
    let mut old_ln = 0usize;
    let mut new_ln = 0usize;
    // Remaining lines of the current hunk, old and new side; a hunk is alive
    // while either count is unspent. Lines without a marker count as context.
    let mut rem: Option<(usize, usize)> = None;

    let push_plain = |rows: &mut Vec<Row>, kind: RowKind, text: &str| {
        rows.push(Row {
            kind,
            text: text.to_string(),
            num: 0,
            toks: None,
        });
    };

    for raw in diff.lines() {
        if raw.starts_with("@@") {
            if let Some((o, oc, n, nc)) = hunk_starts(raw) {
                old_ln = o;
                new_ln = n;
                rem = Some((oc, nc));
            }
            push_plain(&mut rows, RowKind::Hunk, raw);
            continue;
        }
        let hunk_alive = rem.is_some_and(|(ro, rn)| ro > 0 || rn > 0);
        if !hunk_alive && is_header_line(raw) {
            rem = None;
            if let Some(path) = header_path(raw) {
                lexer = if want_lex { Lexer::detect(path) } else { None };
            }
            push_plain(&mut rows, RowKind::Header, raw);
            continue;
        }
        if rem.is_none() {
            if !raw.is_empty() {
                push_plain(&mut rows, RowKind::Header, raw);
            }
            continue;
        }

        let (kind, text, num) = match raw.as_bytes().first() {
            Some(b'+') => {
                let n = new_ln;
                new_ln += 1;
                if let Some((_, rn)) = rem.as_mut() {
                    *rn = rn.saturating_sub(1);
                }
                (RowKind::Add, &raw[1..], n)
            }
            Some(b'-') => {
                let n = old_ln;
                old_ln += 1;
                if let Some((ro, _)) = rem.as_mut() {
                    *ro = ro.saturating_sub(1);
                }
                (RowKind::Del, &raw[1..], n)
            }
            Some(b'\\') => (RowKind::Meta, raw, 0),
            _ if !hunk_alive => {
                rem = None;
                push_plain(&mut rows, RowKind::Header, raw);
                continue;
            }
            _ => {
                // ` ` context; empty and unmarked lines count as context too.
                let n = new_ln;
                old_ln += 1;
                new_ln += 1;
                if let Some((ro, rn)) = rem.as_mut() {
                    *ro = ro.saturating_sub(1);
                    *rn = rn.saturating_sub(1);
                }
                (RowKind::Ctx, raw.strip_prefix(' ').unwrap_or(raw), n)
            }
        };
        let toks = lexer.as_mut().map(|lx| lx.lex_line(text));
        rows.push(Row {
            kind,
            text: text.to_string(),
            num,
            toks,
        });
    }
    rows
}

/// Fold context runs: `CONTEXT` lines around every change stay, the middle of
/// a longer run becomes a `Fold` row. Runs touching a hunk edge keep the
/// `CONTEXT` lines nearest the change.
fn fold_context(rows: Vec<Row>) -> Vec<Row> {
    let n = rows.len();
    let mut keep = vec![true; n];
    // (row index to insert before, hidden line count)
    let mut folds: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if rows[i].kind != RowKind::Ctx {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && rows[i].kind == RowKind::Ctx {
            i += 1;
        }
        let end = i;
        let len = end - start;
        let prev_change = start > 0 && matches!(rows[start - 1].kind, RowKind::Add | RowKind::Del);
        let next_change = end < n && matches!(rows[end].kind, RowKind::Add | RowKind::Del);
        let (head, tail) = match (prev_change, next_change) {
            (true, true) if len > 2 * CONTEXT => (CONTEXT, CONTEXT),
            (true, false) if len > CONTEXT => (CONTEXT, 0),
            (false, true) if len > CONTEXT => (0, CONTEXT),
            _ => continue,
        };
        keep[start + head..end - tail].fill(false);
        folds.push((start + head, len - head - tail));
    }
    if folds.is_empty() {
        return rows;
    }
    let mut out = Vec::with_capacity(n);
    let mut folds = folds.into_iter().peekable();
    for (idx, row) in rows.into_iter().enumerate() {
        if let Some(&(pos, count)) = folds.peek()
            && pos == idx
        {
            out.push(Row {
                kind: RowKind::Fold(count),
                text: String::new(),
                num: 0,
                toks: None,
            });
            folds.next();
        }
        if keep[idx] {
            out.push(row);
        }
    }
    out
}

/// Digits needed for the largest line number in the diff (minimum 1).
fn num_width(rows: &[Row]) -> usize {
    let max = rows
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Add | RowKind::Del | RowKind::Ctx))
        .map(|r| r.num)
        .max()
        .unwrap_or(0);
    let mut w = 1usize;
    let mut v = max;
    while v >= 10 {
        v /= 10;
        w += 1;
    }
    w
}

/// A colour as RGB for band blending: RGB passes through, named ANSI colours
/// resolve to their reference RGB in the table below, anything else is
/// unknown.
fn color_rgb(c: Color) -> Option<(u8, u8, u8)> {
    const NAMED: &[(Color, (u8, u8, u8))] = &[
        (Color::Black, (0, 0, 0)),
        (Color::Red, (128, 0, 0)),
        (Color::Green, (0, 128, 0)),
        (Color::Yellow, (128, 128, 0)),
        (Color::Blue, (0, 0, 128)),
        (Color::Magenta, (128, 0, 128)),
        (Color::Cyan, (0, 128, 128)),
        (Color::Gray, (192, 192, 192)),
        (Color::DarkGray, (128, 128, 128)),
        (Color::LightRed, (255, 0, 0)),
        (Color::LightGreen, (0, 255, 0)),
        (Color::LightYellow, (255, 255, 0)),
        (Color::LightBlue, (0, 0, 255)),
        (Color::LightMagenta, (255, 0, 255)),
        (Color::LightCyan, (0, 255, 255)),
        (Color::White, (255, 255, 255)),
    ];
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        other => NAMED
            .iter()
            .find_map(|(k, rgb)| (*k == other).then_some(*rgb)),
    }
}

/// The full-width band behind an added/removed row: `selected_bg` tinted 25 %
/// toward the diff colour, the same trick `selected_row` uses to stay readable
/// on any palette. `None` when not in truecolor or the colour isn't known.
fn band_style(opts: &RenderOptions, kind: RowKind) -> Option<Style> {
    if opts.color != ColorMode::Truecolor {
        return None;
    }
    let tint = match kind {
        RowKind::Add => opts.theme.green.fg,
        RowKind::Del => opts.theme.red.fg,
        _ => None,
    };
    let (tr, tg, tb) = color_rgb(tint?)?;
    let (br, bg, bb) = opts
        .theme
        .selected_bg
        .bg
        .and_then(color_rgb)
        .unwrap_or((0x1e, 0x24, 0x1c)); // pacode-dark selected_bg
    // 87.5 % base + 12.5 % tint — a faint wash of colour that keeps
    // foregrounds readable on both dark and light palettes.
    let mix = |b: u8, t: u8| ((b as u16 * 7 + t as u16) / 8) as u8;
    Some(Style::default().bg(Color::Rgb(mix(br, tr), mix(bg, tg), mix(bb, tb))))
}

/// Style a code row's spans: syntax tokens, or plain `fg` without a lexer /
/// under `Mono`.
fn code_spans(row: &Row, opts: &RenderOptions) -> Vec<Span<'static>> {
    if opts.color == ColorMode::Mono {
        return vec![Span::styled(row.text.clone(), opts.theme.fg)];
    }
    match &row.toks {
        Some(toks) => toks
            .iter()
            .map(|t| {
                let style = t
                    .kind
                    .map(|k| token_style(&opts.theme, k))
                    .unwrap_or(opts.theme.fg);
                Span::styled(row.text[t.range.clone()].to_string(), style)
            })
            .collect(),
        None => vec![Span::styled(row.text.clone(), opts.theme.fg)],
    }
}

/// Render a unified diff into lines. `render_diff` folds context runs;
/// `render_diff_ex(_, _, true)` shows the whole hunk. Neither wraps — long
/// code is cut with the ellipsis glyph so the number column survives.
pub fn render_diff(diff: &str, opts: &RenderOptions) -> Vec<Line<'static>> {
    render_diff_ex(diff, opts, false)
}

/// Whether collapsed rendering would hide lines — true when `fold_context`
/// emits a `Fold` row, i.e. expanding actually reveals more. The TUI uses
/// this to keep `ctrl+o` pointed only at cells that have something to show.
pub fn diff_hides_lines(diff: &str) -> bool {
    fold_context(parse_rows(diff, false))
        .iter()
        .any(|r| matches!(r.kind, RowKind::Fold(_)))
}

/// `expanded` keeps every context line instead of the `… N lines` marker.
pub fn render_diff_ex(diff: &str, opts: &RenderOptions, expanded: bool) -> Vec<Line<'static>> {
    let rows = parse_rows(diff, opts.color != ColorMode::Mono);
    let nw = num_width(&rows);
    let rows = if expanded { rows } else { fold_context(rows) };

    let band_add = band_style(opts, RowKind::Add);
    let band_del = band_style(opts, RowKind::Del);
    let width = opts.width as usize;
    let minus = if opts.glyphs.ascii { "-" } else { "−" };
    // number column + space + sign + space
    let code_x = nw + 3;
    let code_w = width.saturating_sub(code_x);

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        match row.kind {
            RowKind::Header => {
                out.push(Line::from(Span::styled(row.text.clone(), opts.theme.faint)))
            }
            RowKind::Hunk => out.push(Line::from(Span::styled(row.text.clone(), opts.theme.dim))),
            RowKind::Meta => out.push(Line::from(vec![
                Span::styled(" ".repeat(code_x), opts.theme.faint),
                Span::styled(row.text.clone(), opts.theme.faint),
            ])),
            RowKind::Fold(n) => out.push(Line::from(vec![
                Span::styled(" ".repeat(code_x), opts.theme.faint),
                Span::styled(folded_text(n, opts), opts.theme.dim),
            ])),
            RowKind::Add | RowKind::Del | RowKind::Ctx => {
                let band = match row.kind {
                    RowKind::Add => band_add,
                    RowKind::Del => band_del,
                    RowKind::Ctx
                    | RowKind::Header
                    | RowKind::Hunk
                    | RowKind::Meta
                    | RowKind::Fold(_) => None,
                };
                let sign = match row.kind {
                    RowKind::Add => "+",
                    RowKind::Del => minus,
                    RowKind::Ctx
                    | RowKind::Header
                    | RowKind::Hunk
                    | RowKind::Meta
                    | RowKind::Fold(_) => " ",
                };
                let sign_style = match row.kind {
                    RowKind::Add if opts.color != ColorMode::Mono => opts.theme.green,
                    RowKind::Del if opts.color != ColorMode::Mono => opts.theme.red,
                    RowKind::Add
                    | RowKind::Del
                    | RowKind::Ctx
                    | RowKind::Header
                    | RowKind::Hunk
                    | RowKind::Meta
                    | RowKind::Fold(_) => opts.theme.fg,
                };
                let mut spans = vec![
                    Span::styled(format!("{:>nw$} ", row.num), opts.theme.faint),
                    Span::styled(sign.to_string(), sign_style),
                    Span::raw(" "),
                ];
                spans.extend(truncate_spans(
                    &code_spans(row, opts),
                    code_w,
                    opts.glyphs.ellipsis,
                ));
                if let Some(band) = band {
                    for s in &mut spans {
                        s.style = band.patch(s.style);
                    }
                    let used: usize = spans.iter().map(|s| display_width(&s.content)).sum();
                    if used < width {
                        spans.push(Span::styled(" ".repeat(width - used), band));
                    }
                }
                out.push(Line::from(spans));
            }
        }
    }
    out
}
