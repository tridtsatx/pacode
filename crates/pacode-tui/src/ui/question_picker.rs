//! The question the model asked, as a bottom picker: the header chip, the
//! question, one numbered row per option with its description under it, then
//! the free-text plaque (`index == options.len()` reaches it) and the hints:
//!
//! ```text
//!  Storage · 1/2
//! Where should the cache live?
//!
//!  › 1. Cache dir
//!       the usual place
//!    2. Next to the project
//!       portable
//!
//!    3. Type something.
//!    > enter your answer
//!
//! ↑/↓ select · 1-3 pick · ←/→ question · t type · enter · esc dismiss
//! ```
//!
//! Several questions can wait at once — the `i/n` in the chip and the
//! `←/→ question` hint appear only while the queue holds more than one.
//! When the whole block does not fit the reserved height, the options list
//! windows around the cursor while the plaque and hint stay pinned.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use pacode_render::RenderOptions;

use crate::ui::overlays::selected_row;

/// Lines the free-text plaque and the hint take below the options list:
/// blank separator, `N+1.` label, `> …` input row, blank, hint.
const TAIL_LINES: usize = 5;

/// Rows one option occupies: the numbered label plus one row per wrapped
/// description line (an empty description costs nothing). Shared by `height`
/// and `draw` so the reserved space and the drawn window never disagree.
fn option_height(option: &pacode_types::QuestionOption, width: usize) -> usize {
    if option.description.is_empty() {
        1
    } else {
        1 + pacode_render::wrap_text(&option.description, width.saturating_sub(6).max(1)).len()
    }
}

/// Rows the picker wants at `width`: header chip (also the queue badge's
/// home, so it exists whenever the queue is deeper than one), wrapped
/// question, blank, the option rows, then the plaque block and hint
/// (`TAIL_LINES`). `AppState::bottom_picker_height` caps the result.
pub(crate) fn height(question: &pacode_types::Question, queue_len: usize, width: usize) -> usize {
    let head = usize::from(!question.header.is_empty() || queue_len > 1)
        + pacode_render::wrap_text(&question.question, width.max(1)).len()
        + 1;
    let options: usize = question
        .options
        .iter()
        .map(|o| option_height(o, width))
        .sum();
    head + options + TAIL_LINES
}

/// The tail of `s` that fits `width` cells — a typed answer scrolls left so
/// the end under the cursor stays on screen.
fn tail_to_width(s: &str, width: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;

    if pacode_render::display_width(s) <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for g in s.graphemes(true).rev() {
        let gw = pacode_render::display_width(g);
        if used + gw > width {
            break;
        }
        out.insert_str(0, g);
        used += gw;
    }
    out
}

pub(crate) fn draw(
    frame: &mut Frame,
    area: Rect,
    question: &pacode_types::Question,
    focus: &crate::state::QuestionFocus,
    position: usize,
    queue_len: usize,
    opts: &RenderOptions,
) {
    let index = focus.index;
    let selected = &focus.selected;
    let typed = focus.typed.as_str();
    let typing = focus.typing;
    let width = area.width as usize;
    let count = question.options.len();
    let mut lines: Vec<Line<'static>> = Vec::new();

    if !question.header.is_empty() || queue_len > 1 {
        let chip = if queue_len > 1 {
            if question.header.is_empty() {
                format!(" {position}/{queue_len} ")
            } else {
                format!(" {} · {position}/{queue_len} ", question.header)
            }
        } else {
            format!(" {} ", question.header)
        };
        lines.push(Line::from(Span::styled(chip, opts.theme.selected_bg)));
    }
    for line in pacode_render::wrap_text(&question.question, width.max(1)) {
        lines.push(Line::from(Span::styled(line, opts.theme.bold)));
    }
    lines.push(Line::default());
    let head_h = lines.len();

    // The options window: when the list does not fit, it scrolls so the
    // cursor's option stays visible; the plaque and the hint never scroll.
    let opt_heights: Vec<usize> = question
        .options
        .iter()
        .map(|o| option_height(o, width))
        .collect();
    let budget = (area.height as usize).saturating_sub(head_h + TAIL_LINES);
    let start = crate::ui::overlays::window_groups(
        &opt_heights,
        index.min(count.saturating_sub(1)),
        budget,
    );

    let mut used = 0usize;
    for (i, option) in question.options.iter().enumerate().skip(start) {
        let h = opt_heights[i];
        // Always draw the window's first row even when it overflows the
        // budget — an empty list under a cursor reads as a bug.
        if used > 0 && used + h > budget {
            break;
        }
        used += h;

        let is_cursor = i == index;
        let is_selected = selected.contains(&i);
        let marker = if is_selected {
            opts.glyphs.ok
        } else if is_cursor {
            opts.glyphs.pointer
        } else {
            " "
        };
        let marker_style = if is_selected {
            opts.theme.green
        } else {
            opts.theme.accent
        };
        let suffix_w = if option.recommended {
            " (recommended)".len()
        } else {
            0
        };
        let label = format!(
            "{}. {}",
            i + 1,
            pacode_render::truncate_to_width(
                &option.label,
                width.saturating_sub(3 + suffix_w),
                true,
            )
        );
        let mut spans = vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(
                label,
                if is_cursor {
                    opts.theme.bold
                } else {
                    opts.theme.fg
                },
            ),
        ];
        if option.recommended {
            spans.push(Span::styled(" (recommended)", opts.theme.faint));
        }
        let line = Line::from(spans);
        if is_cursor {
            lines.push(selected_row(line, area.width, opts));
        } else {
            lines.push(line);
        }
        for dl in pacode_render::wrap_text(&option.description, width.saturating_sub(6).max(1)) {
            let dline = Line::from(Span::styled(format!("      {dl}"), opts.theme.dim));
            if is_cursor {
                lines.push(selected_row(dline, area.width, opts));
            } else {
                lines.push(dline);
            }
        }
    }

    // The free-text plaque: a pseudo-option at `index == count`. Its label row
    // stays faint like a separator between the numbered list and the input;
    // the `> ` row is what the cursor and typing land on.
    lines.push(Line::default());
    let on_plaque = index == count;
    lines.push(Line::from(Span::styled(
        format!("   {}. Type something.", count + 1),
        opts.theme.dim,
    )));

    let plaque_row = lines.len();
    let shown = tail_to_width(typed, width.saturating_sub(5));
    let plaque_line = Line::from(vec![
        Span::raw("   "),
        Span::styled("> ", opts.theme.accent),
        if typing {
            Span::styled(shown.clone(), opts.theme.fg)
        } else {
            Span::styled("enter your answer".to_string(), opts.theme.faint)
        },
    ]);
    if on_plaque {
        lines.push(selected_row(plaque_line, area.width, opts));
    } else {
        lines.push(plaque_line);
    }

    lines.push(Line::default());

    let sep = opts.glyphs.dot_sep;
    let arrows = if opts.glyphs.ascii { "^/v" } else { "↑/↓" };
    let side = if opts.glyphs.ascii { "</>" } else { "←/→" };
    let hint = if typing {
        format!("type an answer{sep}enter send{sep}esc back")
    } else {
        let mut h = format!("{arrows} select{sep}1-{} pick{sep}", count + 1);
        if question.multi_select {
            h.push_str(&format!("space toggle{sep}"));
        }
        if queue_len > 1 {
            h.push_str(&format!("{side} question{sep}"));
        }
        h.push_str(&format!("t type{sep}enter{sep}esc dismiss"));
        h
    };
    lines.push(Line::from(Span::styled(
        pacode_render::truncate_to_width(&hint, width, true),
        opts.theme.dim,
    )));

    frame.render_widget(Paragraph::new(lines), area);

    if typing {
        let cx = area.x + 5 + pacode_render::display_width(&shown) as u16;
        let cy = area.y + plaque_row as u16;
        frame.set_cursor_position((
            cx.min(area.right().saturating_sub(1)),
            cy.min(area.bottom().saturating_sub(1)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::{Backend, TestBackend};

    use super::*;

    fn test_question() -> pacode_types::Question {
        pacode_types::Question::new(
            pacode_types::QuestionId::new("qst_1"),
            pacode_types::QuestionOrigin::new(
                pacode_types::AgentId::main(),
                "main",
                pacode_types::CallId::new("call_1"),
            ),
            "Storage",
            "Where should the cache live?",
            vec![
                pacode_types::QuestionOption::new("Cache dir", "the usual place"),
                pacode_types::QuestionOption::new("Next to the project", "portable").recommended(),
            ],
            false,
            0,
        )
        .expect("question")
    }

    /// Draw `draw` on a test backend; returns (buffer text, cursor position).
    /// `queue` is (position, len) — the `i/n` badge only shows past 1.
    #[allow(clippy::too_many_arguments)]
    fn render_question(
        question: &pacode_types::Question,
        index: usize,
        selected: &[usize],
        typed: &str,
        typing: bool,
        queue: (usize, usize),
        width: u16,
        height: u16,
    ) -> (String, Option<(u16, u16)>) {
        let opts = RenderOptions::with_theme(width, false, pacode_render::Theme::ansi());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, width, height);
        let focus = crate::state::QuestionFocus {
            index,
            selected: selected.to_vec(),
            typed: typed.to_string(),
            typing,
        };
        terminal
            .draw(|f| draw(f, area, question, &focus, queue.0, queue.1, &opts))
            .expect("draw");
        let text = format!("{}", terminal.backend());
        let cursor = terminal
            .backend_mut()
            .get_cursor_position()
            .ok()
            .map(|p| (p.x, p.y));
        (text, cursor)
    }

    #[test]
    fn shows_every_option_description_and_the_plaque() {
        let q = test_question();
        let (text, _) = render_question(&q, 0, &[], "", false, (1, 1), 60, 18);
        // Header chip, the question, both options with their descriptions —
        // not only the one under the cursor.
        assert!(text.contains("Storage"), "{text}");
        assert!(text.contains("Where should the cache live?"), "{text}");
        assert!(text.contains("1. Cache dir"), "{text}");
        assert!(text.contains("the usual place"), "{text}");
        assert!(text.contains("2. Next to the project"), "{text}");
        assert!(text.contains("portable"), "{text}");
        // The free-text plaque is numbered past the options.
        assert!(text.contains("3. Type something."), "{text}");
        assert!(text.contains("> enter your answer"), "{text}");
        // Hints mention the plaque's digit range (1-3, not 1-9).
        assert!(text.contains("1-3 pick"), "{text}");
        assert!(text.contains("t type"), "{text}");
    }

    #[test]
    fn a_deeper_queue_badges_the_chip_and_hints_the_walk() {
        let q = test_question();
        let (text, _) = render_question(&q, 0, &[], "", false, (1, 3), 60, 18);
        assert!(text.contains("Storage · 1/3"), "{text}");
        assert!(text.contains("←/→ question"), "{text}");

        // A headerless question still shows the bare position.
        let mut bare = test_question();
        bare.header = String::new();
        let (text, _) = render_question(&bare, 0, &[], "", false, (2, 2), 60, 18);
        assert!(text.contains(" 2/2 "), "{text}");
        // …and at depth one there is no badge and no hint.
        let (text, _) = render_question(&bare, 0, &[], "", false, (1, 1), 60, 18);
        assert!(!text.contains("/1"), "{text}");
        assert!(!text.contains("question"), "{text}");
    }

    #[test]
    fn the_cursor_reaches_the_plaque() {
        let q = test_question();
        // index == options.len() is the plaque; its input row takes the bar.
        let (text, _) = render_question(&q, 2, &[], "", false, (1, 1), 60, 18);
        assert!(text.contains("> enter your answer"), "{text}");

        let opts = RenderOptions::with_theme(60, false, pacode_render::Theme::ansi());
        let backend = TestBackend::new(60, 18);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 60, 18);
        let focus = crate::state::QuestionFocus {
            index: 2,
            ..crate::state::QuestionFocus::default()
        };
        terminal
            .draw(|f| draw(f, area, &q, &focus, 1, 1, &opts))
            .expect("draw");
        // The `> enter your answer` row is the selected bar (ansi selected_bg
        // marks cells REVERSED): find its row and check a padded cell.
        let buf = terminal.backend().buffer().clone();
        let row = (0..18u16)
            .find(|y| {
                (0..60u16)
                    .map(|x| buf[(x, *y)].symbol().to_string())
                    .collect::<String>()
                    .contains("enter your answer")
            })
            .expect("plaque row");
        let cell = buf[(59, row)].clone();
        assert!(
            cell.modifier.contains(ratatui::style::Modifier::REVERSED),
            "plaque row should carry the selected bar"
        );
    }

    #[test]
    fn typing_shows_the_text_and_a_cursor() {
        let q = test_question();
        let (text, cursor) = render_question(&q, 0, &[], "abc", true, (1, 1), 60, 18);
        assert!(text.contains("> abc"), "{text}");
        // Cursor sits after the typed text: `   > ` is 5 cells + 3 chars.
        assert_eq!(cursor, Some((5 + 3, 9)), "{text}");

        // Long input keeps the tail on screen, cursor at the right edge.
        let long = "x".repeat(100);
        let (_text, cursor) = render_question(&q, 0, &[], &long, true, (1, 1), 60, 18);
        assert_eq!(cursor, Some((59, 9)));
    }

    #[test]
    fn height_counts_every_row() {
        let q = test_question();
        // header 1 + question 1 + blank 1 + 2 options × 2 + tail 5 = 12.
        assert_eq!(height(&q, 1, 86), 12);

        // An empty description costs one row, a long one more.
        let mut q2 = test_question();
        q2.options[0].description = String::new();
        assert_eq!(height(&q2, 1, 86), 11);
        q2.options[0].description = "x".repeat(400);
        assert!(height(&q2, 1, 86) > 12);

        // The badge row exists even when the header is empty.
        let mut bare = test_question();
        bare.header = String::new();
        assert_eq!(height(&bare, 1, 86), 11);
        assert_eq!(height(&bare, 3, 86), 12);
    }

    #[test]
    fn scrolls_the_options_window_when_capped() {
        // Six options with descriptions want ~22 rows; a 14-row area must
        // keep the plaque and hint visible and window the list.
        let q = pacode_types::Question::new(
            pacode_types::QuestionId::new("qst_2"),
            pacode_types::QuestionOrigin::new(
                pacode_types::AgentId::main(),
                "main",
                pacode_types::CallId::new("call_2"),
            ),
            "",
            "Pick?",
            (0..6)
                .map(|i| {
                    pacode_types::QuestionOption::new(format!("Option {i}"), format!("desc {i}"))
                })
                .collect(),
            false,
            0,
        )
        .expect("question");
        let (text, _) = render_question(&q, 5, &[], "", false, (1, 1), 60, 14);
        assert!(text.contains("> enter your answer"), "{text}");
        assert!(text.contains("esc dismiss"), "{text}");
        // The window follows the cursor: the last option is shown, the first
        // is scrolled off.
        assert!(text.contains("6. Option 5"), "{text}");
        assert!(!text.contains("1. Option 0"), "{text}");
    }
}
