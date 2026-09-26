//! The rows the `/analyze` page draws.
//!
//! Every line is built to fit the width it is given and then clipped with
//! [`truncate_spans`], so a long tool name or a six-digit token count can
//! never wrap a row into two ragged halves. Nothing here reads a global: the
//! page is a pure function of the report plus the render options.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use pacode_render::RenderOptions;
use pacode_types::metrics::HistogramSnapshot;
use pacode_types::time::{format_duration_ms, format_tokens};
use pacode_types::{
    AnalyzeReport, ContextQualityReport, REQUEST_RECORD_CAP, TOOL_NAME_CAP, TokenCategory,
};

/// Label column, so the five category bars start at the same cell.
const LABEL_W: usize = 14;
/// Bar column, in glyph cells.
const BAR_CELLS: usize = 10;
/// Right-aligned token column.
const VALUE_W: usize = 9;
/// Right-aligned share column, `nnn%` at its widest.
const SHARE_W: usize = 5;
/// Tool rows shown before the rest is left to the scrollback. A session with
/// 64 tool names is pathological; showing all of them would bury every other
/// section under a wall of names.
const MAX_TOOL_ROWS: usize = 20;

/// The whole page, in order.
pub(crate) fn build(
    report: &AnalyzeReport,
    width: usize,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    spend(report, width, opts, &mut lines);
    lines.push(blank());
    tools(report, width, opts, &mut lines);
    lines.push(blank());
    latency(report, width, opts, &mut lines);
    lines.push(blank());
    quality(report, width, opts, &mut lines);
    lines.push(blank());
    coverage(report, width, opts, &mut lines);
    // The width invariant is enforced here rather than in each section: a
    // header, a blank row and whatever the next section adds are all clipped by
    // the same pass, so "no row is wider than the page" cannot be broken by the
    // next person to push a row without `fit`.
    lines
        .into_iter()
        .map(|line| fit(line.spans, width, opts))
        .collect()
}

fn spend(report: &AnalyzeReport, width: usize, opts: &RenderOptions, out: &mut Vec<Line<'static>>) {
    out.push(section("Token spend", opts));
    // The caveat rides with the numbers, not in a footnote: four of these five
    // rows are chars/4, and a reader who misses the label reads them as
    // measured.
    out.push(fit(
        vec![Span::styled(
            "est. chars/4, not a tokenizer; only the reply is a provider count",
            opts.theme.faint,
        )],
        width,
        opts,
    ));
    out.push(fit(
        vec![
            Span::styled("provider reported  ", opts.theme.dim),
            Span::raw(format!(
                "in {} · out {} · cache read {}",
                format_tokens(report.reported_input),
                format_tokens(report.reported_output),
                format_tokens(report.cache_read),
            )),
        ],
        width,
        opts,
    ));
    out.push(blank());

    if report.requests_seen() == 0 {
        out.push(fit(
            vec![Span::styled("no requests recorded yet", opts.theme.dim)],
            width,
            opts,
        ));
        return;
    }

    let total = report.categories.total();
    for (category, tokens) in report.categories.rows() {
        let share = share_of(tokens, total);
        out.push(fit(
            vec![
                Span::styled(pad(category.label(), LABEL_W), opts.theme.dim),
                Span::styled(bar(share, opts), bar_style(category, opts)),
                Span::raw(format!("{:>VALUE_W$}", format_tokens(tokens))),
                Span::styled(
                    format!("{share:>width$}%", width = SHARE_W - 1),
                    opts.theme.faint.patch(if share >= 50.0 {
                        opts.theme.yellow
                    } else {
                        Style::default()
                    }),
                ),
            ],
            width,
            opts,
        ));
    }
    out.push(fit(
        vec![
            Span::styled(pad("total", LABEL_W), opts.theme.bold),
            Span::raw(format_tokens(total)),
            Span::styled(
                format!(" over {} requests", report.requests_seen()),
                opts.theme.faint,
            ),
        ],
        width,
        opts,
    ));
}

fn tools(report: &AnalyzeReport, width: usize, opts: &RenderOptions, out: &mut Vec<Line<'static>>) {
    out.push(section("Tool results by tool", opts));
    if report.tools.is_empty() {
        out.push(fit(
            vec![Span::styled("no tool results yet", opts.theme.dim)],
            width,
            opts,
        ));
        return;
    }
    // The tool name is the part a user cannot guess, so it gets the room and
    // the number right-aligns.
    let name_w = width.saturating_sub(VALUE_W + 12).max(6);
    for (index, tool) in report.tools.iter().take(MAX_TOOL_ROWS).enumerate() {
        let heavy = index == 0;
        out.push(fit(
            vec![
                Span::styled(
                    format!("  {}", pad(&tool.tool, name_w)),
                    if heavy {
                        opts.theme.bold
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("{:>VALUE_W$}", format_tokens(tool.result_tokens)),
                    if heavy {
                        opts.theme.yellow
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(format!(" {} calls", tool.calls), opts.theme.faint),
            ],
            width,
            opts,
        ));
    }
    let hidden = report.tools.len().saturating_sub(MAX_TOOL_ROWS);
    if hidden > 0 {
        out.push(fit(
            vec![Span::styled(
                format!("  {hidden} more tool rows further down"),
                opts.theme.faint,
            )],
            width,
            opts,
        ));
    }
    out.push(fit(
        vec![Span::styled(
            format!(
                "  {} total across {} tool names",
                format_tokens(report.tool_result_tokens),
                report.tools.len()
            ),
            opts.theme.faint,
        )],
        width,
        opts,
    ));
}

fn latency(
    report: &AnalyzeReport,
    width: usize,
    opts: &RenderOptions,
    out: &mut Vec<Line<'static>>,
) {
    out.push(section("Latency per request", opts));
    out.push(fit(
        vec![Span::styled(
            "log2 histogram, the same quantiles pacode stats prints",
            opts.theme.faint,
        )],
        width,
        opts,
    ));
    if report.total_latency.count == 0 {
        out.push(fit(
            vec![Span::styled("no completed requests yet", opts.theme.dim)],
            width,
            opts,
        ));
        return;
    }
    out.push(fit(
        histogram("  first token", &report.ttft, opts),
        width,
        opts,
    ));
    out.push(fit(
        histogram("  total     ", &report.total_latency, opts),
        width,
        opts,
    ));
}

fn histogram(label: &str, snap: &HistogramSnapshot, opts: &RenderOptions) -> Vec<Span<'static>> {
    vec![
        Span::styled(label.to_string(), opts.theme.dim),
        Span::raw(format!(" n={} ", snap.count)),
        Span::raw(format!("p50 {} · ", ms(snap.quantile_micros(0.50)))),
        Span::raw(format!("p90 {} · ", ms(snap.quantile_micros(0.90)))),
        Span::raw(format!("p99 {}", ms(snap.quantile_micros(0.99)))),
    ]
}

fn quality(
    report: &AnalyzeReport,
    width: usize,
    opts: &RenderOptions,
    out: &mut Vec<Line<'static>>,
) {
    out.push(section("Context quality", opts));
    let ContextQualityReport {
        checkpoints,
        passed,
        ignored,
        wrong,
        lost_to_compaction,
        score,
    } = report.quality;
    if checkpoints == 0 {
        out.push(fit(
            vec![Span::styled("no checkpoints run yet", opts.theme.dim)],
            width,
            opts,
        ));
        return;
    }
    out.push(fit(
        vec![
            Span::raw(format!("{checkpoints} checkpoints · ")),
            Span::styled(
                format!("{passed} passed"),
                opts.theme.green.patch(opts.theme.bold),
            ),
            Span::raw(format!(" · {ignored} ignored · {wrong} wrong · ")),
            // Counted on its own, never folded into "wrong": a word compaction
            // removed is not something the model can fail.
            Span::styled(
                format!("{lost_to_compaction} lost to compaction"),
                opts.theme.dim,
            ),
        ],
        width,
        opts,
    ));
    if report.quality.is_unscored() {
        out.push(fit(
            vec![Span::styled(
                "score: not scored, every checkpoint left the context first",
                opts.theme.faint,
            )],
            width,
            opts,
        ));
        return;
    }
    let percent = (score * 100.0).clamp(0.0, 100.0);
    out.push(fit(
        vec![
            Span::raw("score "),
            Span::styled(
                format!("{percent:.0}%"),
                opts.theme.bold.patch(score_style(percent, opts)),
            ),
            Span::styled(
                format!(
                    "  ({} of {} checkpoints scored)",
                    passed,
                    report.quality.scored_checkpoints()
                ),
                opts.theme.faint,
            ),
        ],
        width,
        opts,
    ));
}

fn coverage(
    report: &AnalyzeReport,
    width: usize,
    opts: &RenderOptions,
    out: &mut Vec<Line<'static>>,
) {
    out.push(section("Coverage", opts));
    let detailed = report.requests_detailed();
    if report.dropped_requests > 0 {
        // Visible on purpose. A per-request tail that looked like a whole
        // session would be a number the page cannot defend.
        out.push(fit(
            vec![
                Span::raw(format!(
                    "{detailed} of {} requests detailed",
                    report.requests_seen()
                )),
                Span::styled(
                    format!(
                        " · {} older dropped at the {REQUEST_RECORD_CAP} cap",
                        report.dropped_requests
                    ),
                    opts.theme.yellow,
                ),
            ],
            width,
            opts,
        ));
        out.push(fit(
            vec![Span::styled(
                "  session totals above still cover every request",
                opts.theme.faint,
            )],
            width,
            opts,
        ));
    } else {
        out.push(fit(
            vec![Span::styled(
                format!("all {detailed} requests detailed"),
                opts.theme.faint,
            )],
            width,
            opts,
        ));
    }
    if report.folded_tools > 0 {
        out.push(fit(
            vec![Span::styled(
                format!(
                    "{} tool calls folded into one row at the {TOOL_NAME_CAP}-name cap",
                    report.folded_tools
                ),
                opts.theme.faint,
            )],
            width,
            opts,
        ));
    }
}

fn share_of(tokens: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (tokens as f64 / total as f64) * 100.0
    }
}

fn bar(share: f64, opts: &RenderOptions) -> String {
    let filled = (((share / 100.0) * BAR_CELLS as f64).round() as usize).min(BAR_CELLS);
    let mut bar = opts.glyphs.bar_full.repeat(filled);
    bar.push_str(&opts.glyphs.bar_empty.repeat(BAR_CELLS - filled));
    bar
}

fn bar_style(category: TokenCategory, opts: &RenderOptions) -> Style {
    match category {
        TokenCategory::Instructions => opts.theme.accent,
        TokenCategory::ToolDefs => opts.theme.cyan,
        TokenCategory::History => opts.theme.violet,
        // The row this page was written for: the flood, in the warning colour.
        TokenCategory::ToolResults => opts.theme.yellow,
        TokenCategory::Reply => opts.theme.dim,
    }
}

fn score_style(percent: f64, opts: &RenderOptions) -> Style {
    if percent >= 80.0 {
        opts.theme.green
    } else if percent >= 50.0 {
        opts.theme.yellow
    } else {
        opts.theme.red
    }
}

fn ms(micros: u64) -> String {
    format_duration_ms(micros / 1_000)
}

fn pad(text: &str, width: usize) -> String {
    let len = pacode_render::display_width(text);
    if len >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - len))
}

fn section(title: &str, opts: &RenderOptions) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        opts.theme.bold.patch(opts.theme.accent),
    ))
}

fn blank() -> Line<'static> {
    Line::default()
}

/// Clip to `width` rather than wrap: a wrapped row breaks the bar column and
/// reads as two rows of data.
fn fit(spans: Vec<Span<'static>>, width: usize, opts: &RenderOptions) -> Line<'static> {
    Line::from(pacode_render::truncate_spans(
        &spans,
        width,
        opts.glyphs.ellipsis,
    ))
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod rows_tests;
