//! The transcript (main dialog and the agent panel share this renderer).

use std::collections::{HashSet, VecDeque};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::cache::CacheKey;
use pacode_render::markdown::split_stable_tail;
use pacode_render::{
    LineCache, RenderOptions, display_width, render_markdown, truncate_to_width, wrap_text,
};
use pacode_types::transcript::ToolStatus;
use pacode_types::{ExplainLevel, RiskLevel, ToastLevel, TranscriptKind};

use crate::state::transcript::{Cell, CellKind, HeaderInfo, PermExplain, Transcript};

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod dialog_tests;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    transcript: &mut Transcript,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let width = area.width;
    // Disjoint field borrows: the rendered blocks borrow the cache for the
    // whole draw, so `&mut transcript` helpers (record_render) cannot run while
    // they live — their writes go through the field bindings instead.
    let Transcript {
        cells,
        expanded,
        cache,
        header,
        cell_lines,
        rendered_lines,
        viewport_height,
        scroll_from_bottom,
        first_visible_line,
        live_cell,
        ..
    } = &mut *transcript;

    let blocks = build_cell_lines(
        CellsSource {
            cells,
            expanded,
            header: header.as_ref(),
            live_id: *live_cell,
            width,
            anim_frame,
            welcome_ms,
        },
        cache,
        opts,
    );

    // Remember where each cell landed, so a click on a row finds its cell. The
    // header block, when there is one, is not a cell and gets no entry.
    let header_blocks = usize::from(header.is_some());
    let mut spans = Vec::with_capacity(cells.len());
    let mut cursor = 0usize;
    for (i, block) in blocks.iter().enumerate() {
        let end = cursor + block.lines().len();
        if i >= header_blocks
            && let Some(cell) = cells.get(i - header_blocks)
        {
            spans.push((cell.id, cursor, end));
        }
        cursor = end;
    }
    *cell_lines = spans;

    let total_lines: usize = blocks.iter().map(|b| b.lines().len()).sum();
    let viewport = area.height as usize;

    // record_render inlined: the cached-line borrows above rule out `&mut self`.
    *rendered_lines = Some(total_lines);
    *viewport_height = Some(viewport);
    let max_scroll = total_lines.saturating_sub(viewport);
    *scroll_from_bottom = (*scroll_from_bottom).min(max_scroll);
    *first_visible_line = max_scroll.saturating_sub(*scroll_from_bottom);

    let (start_line, is_scrolled_up) = if total_lines <= viewport {
        (0, false)
    } else {
        let scroll = *scroll_from_bottom;
        let start = max_scroll.saturating_sub(scroll);
        (start, scroll > 0)
    };

    // Only the viewport window is cloned into the Paragraph — cached blocks
    // stay borrowed, so a one-line redraw never copies the whole transcript.
    let mut visible_lines: Vec<Line<'static>> = Vec::with_capacity(viewport);
    let mut current_idx = 0usize;

    'collect: for block in &blocks {
        for line in block.lines() {
            if current_idx >= start_line && visible_lines.len() < viewport {
                visible_lines.push(line.clone());
            }
            if visible_lines.len() >= viewport && current_idx >= start_line {
                break 'collect;
            }
            current_idx += 1;
        }
    }

    if is_scrolled_up && !visible_lines.is_empty() {
        let last_idx = visible_lines.len() - 1;
        let msg = format!("↓ {} new lines", *scroll_from_bottom);
        visible_lines[last_idx] = Line::from(Span::styled(msg, opts.theme.dim));
    }

    frame.render_widget(Paragraph::new(visible_lines), area);
}

/// One rendered block of the transcript. Cached cells stay borrowed from the
/// line cache — only the cells that must render fresh own their lines.
enum Block<'a> {
    Cached(&'a [Line<'static>]),
    Owned(Vec<Line<'static>>),
}

impl Block<'_> {
    fn lines(&self) -> &[Line<'static>] {
        match self {
            Block::Cached(lines) => lines,
            Block::Owned(lines) => lines,
        }
    }
}

/// How a cell's block is produced in the second phase.
enum CellPlan {
    /// Served from `cache` under this key; phase one made sure it is there.
    Cached(CacheKey),
    /// The live assistant cell: its stable part is cached, its tail renders
    /// fresh every frame.
    LiveAssistant,
    /// Rendered fresh and never cached: running tool calls carry the spinner.
    Fresh,
}

/// Everything `build_cell_lines` reads out of the transcript, as one borrow
/// group: the cache and the render options stay separate because the cache is
/// the `&mut` half and the options outlive the frame.
struct CellsSource<'a> {
    cells: &'a VecDeque<Cell>,
    expanded: &'a HashSet<u64>,
    header: Option<&'a HeaderInfo>,
    live_id: Option<u64>,
    width: u16,
    anim_frame: u64,
    welcome_ms: u64,
}

/// Render every block of the transcript in display order: the header banner
/// first (it lives outside `cells` so a transcript seq can never collide with
/// it), then one block per cell.
///
/// §8.1: the cells and the render cache are borrowed as separate fields —
/// the previous per-frame `CellKind` clone of every cell (each potentially a
/// whole markdown document) is gone. The per-frame `to_vec` of every cached
/// cell is gone too, in two phases: first a `&mut` pass makes sure every
/// cacheable cell is in the cache (rendering misses, refreshing recency), then
/// a `&` pass collects the blocks as borrows. Live assistant cells and running
/// tool calls still render fresh every frame and stay `Owned`.
fn build_cell_lines<'a>(
    src: CellsSource<'a>,
    cache: &'a mut LineCache,
    opts: &RenderOptions,
) -> Vec<Block<'a>> {
    let CellsSource {
        cells,
        expanded,
        header,
        live_id,
        width,
        anim_frame,
        welcome_ms,
    } = src;
    let mut blocks: Vec<Block<'a>> = Vec::with_capacity(cells.len() + 1);

    if let Some(info) = header {
        blocks.push(Block::Owned(crate::ui::header::render(
            info, width, opts, anim_frame, welcome_ms,
        )));
    }

    // Phase one, `&mut cache`: plan every cell and fill cache misses.
    let mut plans: Vec<CellPlan> = Vec::with_capacity(cells.len());
    for cell in cells.iter() {
        plans.push(plan_cell(
            cell,
            live_id == Some(cell.id),
            expanded,
            opts,
            cache,
            width,
            anim_frame,
        ));
    }

    // Phase two, `& cache`: no more `&mut` calls, so the cached blocks can be
    // borrowed out of the entries phase one left behind.
    let cache: &'a LineCache = cache;
    for (cell, plan) in cells.iter().zip(plans.iter()) {
        blocks.push(cell_block(
            cell,
            plan,
            expanded.contains(&cell.id),
            opts,
            cache,
            width,
            anim_frame,
        ));
    }

    blocks
}

/// The phase-one decision for a cell: where its lines come from, making sure
/// the cache holds whatever phase two will borrow.
fn plan_cell(
    cell: &Cell,
    is_live: bool,
    expanded_set: &HashSet<u64>,
    opts: &RenderOptions,
    cache: &mut LineCache,
    width: u16,
    anim_frame: u64,
) -> CellPlan {
    // An expanded cell renders differently, so it is a different cache entry
    // rather than a stale one.
    let expanded = expanded_set.contains(&cell.id);
    let expansion_bit = if expanded { 0x4000_0000_0000_0000 } else { 0 };
    let key = CacheKey {
        cell: cell.id ^ ((cell.version as u64) << 48) ^ expansion_bit,
        width,
    };

    let is_running_tool = matches!(
        cell.kind,
        CellKind::Item(TranscriptKind::ToolCall {
            status: ToolStatus::Running,
            ..
        })
    );

    if is_running_tool {
        return CellPlan::Fresh;
    }

    if !is_live {
        if cache.get(key).is_none() {
            let rendered = render_cell(cell, width, opts, anim_frame, expanded);
            cache.insert(key, rendered);
        }
        return CellPlan::Cached(key);
    }

    if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind {
        // The stable prefix of the live answer is cacheable; the tail is not.
        let (stable, _) = split_stable_tail(text);
        let stable_key = CacheKey {
            cell: (cell.id << 1) ^ (stable.len() as u64) ^ 0x8000_0000_0000_0000,
            width,
        };
        if cache.get(stable_key).is_none() {
            cache.insert(stable_key, render_markdown(stable, opts));
        }
        return CellPlan::LiveAssistant;
    }

    // A live cell that is not an answer still caches; the version in the key
    // keeps pace with its updates.
    if cache.get(key).is_none() {
        let rendered = render_cell(cell, width, opts, anim_frame, expanded);
        cache.insert(key, rendered);
    }
    CellPlan::Cached(key)
}

/// The phase-two block for a cell: a borrow of what phase one cached, or the
/// fresh render for the cells that draw anew every frame.
fn cell_block<'a>(
    cell: &Cell,
    plan: &CellPlan,
    expanded: bool,
    opts: &RenderOptions,
    cache: &'a LineCache,
    width: u16,
    anim_frame: u64,
) -> Block<'a> {
    match plan {
        CellPlan::Cached(key) => match cache.peek(*key) {
            Some(lines) => Block::Cached(lines),
            // Phase one inserted it, so a miss cannot happen — render anyway
            // rather than panic if the invariant is ever broken.
            None => Block::Owned(render_cell(cell, width, opts, anim_frame, expanded)),
        },
        CellPlan::LiveAssistant => {
            if let CellKind::Item(TranscriptKind::Assistant { text, .. }) = &cell.kind {
                let (stable, tail) = split_stable_tail(text);
                let stable_key = CacheKey {
                    cell: (cell.id << 1) ^ (stable.len() as u64) ^ 0x8000_0000_0000_0000,
                    width,
                };

                let mut combined = match cache.peek(stable_key) {
                    Some(lines) => lines.to_vec(),
                    None => render_markdown(stable, opts),
                };
                let mut tail_lines = render_markdown(tail, opts);
                combined.append(&mut tail_lines);
                if let Some(stats) = cell.stats.as_deref() {
                    combined.push(Line::from(Span::styled(
                        stats.to_string(),
                        opts.theme.faint,
                    )));
                }
                combined.push(Line::default());
                return Block::Owned(combined);
            }
            // A live non-assistant cell is planned Cached, so this is
            // unreachable — still render rather than panic.
            Block::Owned(render_cell(cell, width, opts, anim_frame, expanded))
        }
        CellPlan::Fresh => Block::Owned(render_cell(cell, width, opts, anim_frame, expanded)),
    }
}

/// The transcript as plain text, one entry per rendered line, in the same order
/// and with the same indices the selection uses. Rendering goes through the same
/// line cache as drawing, so this costs a walk of already-rendered lines.
pub(crate) fn plain_lines(
    transcript: &mut Transcript,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    welcome_ms: u64,
) -> Vec<String> {
    let Transcript {
        cells,
        expanded,
        cache,
        header,
        live_cell,
        ..
    } = &mut *transcript;
    build_cell_lines(
        CellsSource {
            cells,
            expanded,
            header: header.as_ref(),
            live_id: *live_cell,
            width,
            anim_frame,
            welcome_ms,
        },
        cache,
        opts,
    )
    .iter()
    .flat_map(|block| block.lines())
    .map(|line| {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    })
    .collect()
}

/// Test-only entry point: render one cell exactly as the transcript would.
#[cfg(test)]
pub(crate) fn render_cell_for_test(
    cell: &Cell,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    render_cell(cell, width, opts, 0, false)
}

fn render_cell(
    cell: &Cell,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    match &cell.kind {
        CellKind::Gap => vec![Line::default()],
        CellKind::Item(kind) => render_item_inner(
            kind,
            cell.perm_explain.as_ref(),
            cell.stats.as_deref(),
            width,
            opts,
            anim_frame,
            expanded,
        ),
        CellKind::BackgroundResult(result) => render_background_result(result, width, opts),
    }
}

/// A finished background job, drawn in the tool-call idiom: symbol, what ran,
/// then the outcome with its duration and exit code.
fn render_background_result(
    result: &crate::state::BackgroundResult,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    use crate::state::{BackgroundKind, BackgroundOutcome};

    let noun = match result.kind {
        BackgroundKind::Task => "Background",
        BackgroundKind::Agent => "Agent",
    };
    let dur = pacode_types::time::format_duration_ms(result.duration_ms);
    let (word, style) = match result.outcome {
        BackgroundOutcome::Completed => ("done", opts.theme.green),
        BackgroundOutcome::Failed => ("failed", opts.theme.red),
        BackgroundOutcome::Killed => ("killed", opts.theme.yellow),
    };
    let mut tail = format!("{word} {dur}");
    if let Some(code) = result.exit_code
        && code != 0
    {
        tail.push_str(&format!(" · exit {code}"));
    }

    // The label is the part that can be arbitrarily long, so it is the part that
    // gets an ellipsis: the outcome must stay readable at any width.
    let sym = opts.glyphs.tool;
    let head = format!("{sym} {noun} ");
    let fixed = pacode_render::display_width(&head) + pacode_render::display_width(&tail) + 1;
    let label = truncate_to_width(&result.label, (width as usize).saturating_sub(fixed), true);

    vec![Line::from(vec![
        Span::styled(head, opts.theme.cyan),
        Span::styled(label, opts.theme.dim),
        Span::raw(" "),
        Span::styled(tail, style),
    ])]
}

/// Render one item in its folded form. Only tests reach for this directly; the
/// transcript goes through the cache, which knows whether the cell is expanded.
#[cfg(test)]
pub(crate) fn render_item(
    kind: &TranscriptKind,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
) -> Vec<Line<'static>> {
    render_item_inner(kind, None, stats, width, opts, anim_frame, false)
}

/// `expanded` shows a tool call's whole output, wrapped, instead of its first
/// few lines cut to the width. `perm_explain` rides along for permission
/// cells — the `e`-key answer is cell state, not part of the wire item.
fn render_item_inner(
    kind: &TranscriptKind,
    perm_explain: Option<&PermExplain>,
    stats: Option<&str>,
    width: u16,
    opts: &RenderOptions,
    anim_frame: u64,
    expanded: bool,
) -> Vec<Line<'static>> {
    match kind {
        TranscriptKind::User { text } => {
            let bar = if opts.glyphs.ascii { "|" } else { "▎" };
            let prefix = format!("{bar} ");
            let avail = (width as usize).saturating_sub(2).max(1);
            let lines = wrap_text(text, avail);
            let mut out = Vec::new();
            for (i, l) in lines.into_iter().enumerate() {
                let style = if i == 0 {
                    opts.theme.bold
                } else {
                    opts.theme.fg
                };
                out.push(Line::from(vec![
                    Span::styled(prefix.clone(), opts.theme.user_bar),
                    Span::styled(l, style),
                ]));
            }
            out.push(Line::default());
            out
        }
        TranscriptKind::Assistant { text, complete } => {
            if text.is_empty() && !complete {
                return vec![
                    Line::from(Span::styled("…", opts.theme.faint)),
                    Line::default(),
                ];
            }
            let mut lines = render_markdown(text, opts);
            if let Some(stats) = stats {
                lines.push(Line::from(Span::styled(
                    stats.to_string(),
                    opts.theme.faint,
                )));
            }
            lines.push(Line::default());
            lines
        }
        TranscriptKind::Reasoning { text, complete } => {
            if !opts.thinking {
                return Vec::new();
            }
            let line = if *complete {
                Line::from(Span::styled("thought for a moment", opts.theme.faint))
            } else if text.is_empty() {
                Line::from(Span::styled("thinking…", opts.theme.faint))
            } else {
                Line::from(Span::styled(
                    format!(
                        "thinking: {}",
                        truncate_to_width(text, (width as usize).saturating_sub(11), true)
                    ),
                    opts.theme.faint,
                ))
            };
            vec![line, Line::default()]
        }
        TranscriptKind::ToolCall {
            call_id: _,
            name,
            title,
            intent: _,
            status,
            preview,
            diff,
            duration_ms,
            task: _,
        } => crate::ui::toolcell::render(
            &crate::ui::toolcell::ToolCallView {
                name,
                title,
                status: *status,
                preview,
                diff: diff.as_ref(),
                duration_ms: *duration_ms,
            },
            width,
            opts,
            anim_frame,
            expanded,
        ),
        TranscriptKind::Notice { level, text } => {
            let style = match level {
                ToastLevel::Success => opts.theme.green,
                ToastLevel::Warn => opts.theme.accent,
                ToastLevel::Error => opts.theme.red,
                ToastLevel::Info => opts.theme.faint,
            };
            if *level == ToastLevel::Error {
                crate::ui::notice::render_error(text, width, opts)
            } else {
                let mut out: Vec<Line<'static>> = wrap_text(text, (width as usize).max(1))
                    .into_iter()
                    .map(|l| Line::from(Span::styled(l, style)))
                    .collect();
                out.push(Line::default());
                out
            }
        }
        TranscriptKind::Permission(req) => {
            // The static classifier's risk rides the header as a chip; the
            // title yields width first so the chip can never be cut off.
            let head = format!("{} permission: ", opts.glyphs.chevrons);
            let chip = req
                .risk
                .map(|r| (format!("risk: {}", risk_label(r)), risk_style(r, opts)));
            let chip_w = chip
                .as_ref()
                .map(|(t, _)| display_width(t) + display_width(opts.glyphs.dot_sep) + 2)
                .unwrap_or(0);
            let title_w = (width as usize)
                .saturating_sub(display_width(&head) + chip_w)
                .max(1);
            let mut head_spans = vec![
                Span::styled(head, opts.theme.accent),
                Span::styled(
                    truncate_to_width(&req.title, title_w, true),
                    opts.theme.bold,
                ),
            ];
            if let Some((text, style)) = chip {
                head_spans.push(Span::styled(
                    format!(" {} ", opts.glyphs.dot_sep),
                    opts.theme.faint,
                ));
                head_spans.push(Span::styled(text, style));
            }
            let mut lines = vec![Line::from(head_spans)];
            if !req.detail.is_empty() {
                for dl in req.detail.lines().take(4) {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "    {}",
                            truncate_to_width(dl, (width as usize).saturating_sub(4), true)
                        ),
                        opts.theme.dim,
                    )));
                }
            }
            match perm_explain {
                // `e` pressed — the aux answer is still in flight.
                Some(PermExplain::Requested) => lines.push(Line::from(Span::styled(
                    "    explain requested…".to_string(),
                    opts.theme.faint,
                ))),
                // The aux model's answer landed — display-only, the verdict
                // keys below stay live while it is read.
                Some(PermExplain::Done { text, level }) => {
                    lines.extend(render_explain_lines(text, *level, width, opts));
                }
                None => {}
            }
            lines.push(Line::from(Span::styled(
                "y allow · a session · n deny · e explain",
                opts.theme.cyan,
            )));
            lines.push(Line::default());
            lines
        }
        TranscriptKind::Question { question, answer } => {
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{} ", opts.glyphs.chevrons), opts.theme.accent),
                Span::styled(question.header.clone(), opts.theme.selected_bg),
            ])];
            for line in pacode_render::wrap_text(&question.question, width as usize) {
                lines.push(Line::from(Span::styled(line, opts.theme.bold)));
            }

            match answer {
                // Unanswered: the picker below is showing the options and the
                // keys, so repeating them here would put the same card on screen
                // twice.
                None => lines.push(Line::from(Span::styled(
                    "waiting for your answer",
                    opts.theme.faint,
                ))),
                Some(a) if a.cancelled => {
                    lines.push(Line::from(Span::styled("dismissed", opts.theme.faint)));
                }
                Some(a) => {
                    for (i, option) in question.options.iter().enumerate() {
                        let chosen = a.selected.contains(&i);
                        let marker = if chosen { opts.glyphs.ok } else { " " };
                        let mut label = format!("{}. {}", i + 1, option.label);
                        if option.recommended {
                            label.push_str(" (recommended)");
                        }
                        lines.push(Line::from(vec![
                            Span::styled(format!("{marker} "), opts.theme.green),
                            Span::styled(
                                truncate_to_width(&label, (width as usize).saturating_sub(3), true),
                                if chosen {
                                    opts.theme.bold
                                } else {
                                    opts.theme.dim
                                },
                            ),
                        ]));
                        // The chosen option keeps its description on record —
                        // the picker's "what this means" is part of the answer.
                        if chosen && !option.description.is_empty() {
                            for dl in wrap_text(
                                &option.description,
                                (width as usize).saturating_sub(4).max(1),
                            ) {
                                lines.push(Line::from(Span::styled(
                                    format!("    {dl}"),
                                    opts.theme.dim,
                                )));
                            }
                        }
                    }
                    if let Some(text) = a.free_text.as_ref().filter(|t| !t.trim().is_empty()) {
                        for (j, tl) in wrap_text(text, (width as usize).saturating_sub(2).max(1))
                            .into_iter()
                            .enumerate()
                        {
                            let prefix = if j == 0 { "> " } else { "  " };
                            lines.push(Line::from(Span::styled(
                                format!("{prefix}{tl}"),
                                opts.theme.faint,
                            )));
                        }
                    }
                }
            }
            lines.push(Line::default());
            lines
        }
        TranscriptKind::BashCommand {
            command,
            output,
            exit_code,
            truncated: _,
        } => {
            let mut out = Vec::new();
            let mut header_spans = vec![
                Span::styled(
                    "! ",
                    opts.theme
                        .accent
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(command.clone(), opts.theme.bold),
            ];

            if let Some(code) = exit_code
                && *code != 0
            {
                header_spans.push(Span::raw(" "));
                header_spans.push(Span::styled(format!("(exit {code})"), opts.theme.red));
            }
            out.push(Line::from(header_spans));

            if !output.is_empty() {
                for line in output.lines() {
                    out.push(Line::from(vec![
                        Span::styled("  │ ", opts.theme.dim),
                        Span::styled(
                            truncate_to_width(line, (width as usize).saturating_sub(4), true),
                            opts.theme.fg,
                        ),
                    ]));
                }
            }

            out.push(Line::default());
            out
        }
    }
}

/// The `e`-key answer under a pending permission: `explain · <level>:` in
/// the rating's colour, then the aux model's text dim-wrapped beneath it.
/// `level: None` is the daemon's honest-failure path — the label drops the
/// rating word and the text says what went wrong. Capped at a few lines: the
/// daemon already caps the answer at ~2k chars, but a narrow pane must not
/// let it push the verdict hints off the bottom of the cell.
fn render_explain_lines(
    text: &str,
    level: Option<ExplainLevel>,
    width: u16,
    opts: &RenderOptions,
) -> Vec<Line<'static>> {
    /// The most wrapped lines a permission explanation may occupy.
    const EXPLAIN_MAX_LINES: usize = 8;
    let indent = "    ";
    let mut label = vec![Span::styled(format!("{indent}explain"), opts.theme.faint)];
    if let Some(lv) = level {
        label.push(Span::styled(
            opts.glyphs.dot_sep.to_string(),
            opts.theme.faint,
        ));
        label.push(Span::styled(
            crate::state::transcript::explain_level_name(lv),
            explain_level_style(lv, opts),
        ));
    }
    label.push(Span::styled(": ".to_string(), opts.theme.faint));
    let label_w: usize = label.iter().map(|s| display_width(&s.content)).sum();
    let text_w = (width as usize).saturating_sub(label_w).max(1);
    let wrapped = wrap_text(text, text_w);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut first = label;
    if let Some(head) = wrapped.first() {
        first.push(Span::styled(head.clone(), opts.theme.dim));
    }
    out.push(Line::from(first));
    for wl in wrapped.iter().skip(1).take(EXPLAIN_MAX_LINES - 1) {
        out.push(Line::from(Span::styled(
            format!("{indent}{wl}"),
            opts.theme.dim,
        )));
    }
    let hidden = wrapped.len().saturating_sub(EXPLAIN_MAX_LINES);
    if hidden > 0 {
        out.push(Line::from(Span::styled(
            format!(
                "{indent}{} {hidden} more {}",
                opts.glyphs.ellipsis,
                pacode_render::text::lines_noun(hidden)
            ),
            opts.theme.faint,
        )));
    }
    out
}

/// Colour for the aux model's danger rating: `none` stays quiet, `low` is
/// fine, `medium` asks attention, `high` is red, `catastrophic` shouts.
fn explain_level_style(level: ExplainLevel, opts: &RenderOptions) -> ratatui::style::Style {
    match level {
        ExplainLevel::None => opts.theme.faint,
        ExplainLevel::Low => opts.theme.green,
        ExplainLevel::Medium => opts.theme.yellow,
        ExplainLevel::High => opts.theme.red,
        ExplainLevel::Catastrophic => opts.theme.red.add_modifier(ratatui::style::Modifier::BOLD),
    }
}

/// The `risk: <word>` chip on a permission header — the static classifier's
/// vocabulary (`pacode-risk`), kept lowercase like the explain rating.
fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Safe => "safe",
        RiskLevel::Low => "low",
        RiskLevel::Confirm => "confirm",
        RiskLevel::Catastrophic => "catastrophic",
    }
}

/// Same colour ladder as [`explain_level_style`], mapped onto the static
/// risk classes: `safe` is quiet, `low` fine, `confirm` needs a look, and
/// `catastrophic` shouts.
fn risk_style(risk: RiskLevel, opts: &RenderOptions) -> ratatui::style::Style {
    match risk {
        RiskLevel::Safe => opts.theme.faint,
        RiskLevel::Low => opts.theme.green,
        RiskLevel::Confirm => opts.theme.yellow,
        RiskLevel::Catastrophic => opts.theme.red.add_modifier(ratatui::style::Modifier::BOLD),
    }
}
