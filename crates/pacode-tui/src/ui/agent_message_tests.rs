use pacode_render::{ColorMode, RenderOptions, Theme, display_width};
use pacode_types::{AgentMessageDirection, AgentReply};
use ratatui::text::Line;

use super::{AgentMessageView, render};

fn opts(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::truecolor(), ColorMode::Truecolor)
}

fn rows(view: &AgentMessageView<'_>, width: u16, expanded: bool) -> Vec<String> {
    render(view, &opts(width), expanded)
        .iter()
        .map(|line: &Line<'_>| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn view<'a>(
    direction: AgentMessageDirection,
    text: &'a str,
    reply: Option<&'a AgentReply>,
) -> AgentMessageView<'a> {
    AgentMessageView {
        subagent_name: "tests",
        direction,
        text,
        reply,
    }
}

#[test]
fn a_send_is_one_line_naming_the_subagent() {
    let rows = rows(
        &view(
            AgentMessageDirection::ToSubagent,
            "also cover the empty case\nand the rest",
            None,
        ),
        60,
        false,
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0], "→ tests: also cover the empty case…");
    assert!(rows[1].is_empty());
}

#[test]
fn an_open_question_says_it_is_waiting() {
    let rows = rows(
        &view(AgentMessageDirection::Question, "sqlite or postgres?", None),
        60,
        false,
    );
    assert_eq!(rows[0], "? tests asks: sqlite or postgres?");
    assert_eq!(rows[1], "  └ waiting for an answer");
}

#[test]
fn a_question_shows_how_it_ended() {
    let answered = AgentReply::Answered {
        text: "postgres".into(),
    };
    let cases = [
        (&answered, "  └ answer: postgres"),
        (
            &AgentReply::Unanswered,
            "  └ no answer: proceeding on its own judgment",
        ),
        (
            &AgentReply::Failed,
            "  └ no answer: the orchestrator's turn failed",
        ),
        (&AgentReply::Cancelled, "  └ cancelled"),
    ];
    for (reply, expected) in cases {
        let rows = rows(
            &view(AgentMessageDirection::Question, "which?", Some(reply)),
            60,
            false,
        );
        assert_eq!(rows[1], expected);
    }
}

#[test]
fn folded_rows_fit_the_width_and_expanded_shows_everything() {
    let long = "word ".repeat(40);
    let answer = AgentReply::Answered { text: long.clone() };
    let narrow = render(
        &view(AgentMessageDirection::Question, &long, Some(&answer)),
        &opts(30),
        false,
    );
    for line in &narrow {
        let width: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
        assert!(width <= 30, "row wider than the pane: {line:?}");
    }
    assert_eq!(narrow.len(), 3);

    let expanded = rows(
        &view(AgentMessageDirection::Question, &long, Some(&answer)),
        30,
        true,
    );
    assert!(expanded.len() > 6, "{expanded:?}");
    assert_eq!(
        expanded.iter().filter(|r| r.contains("word")).count(),
        expanded.len() - 3,
        "header, answer label and the blank row are the only rows without text"
    );
}

/// A long subagent name, wide characters included, never pushes the header
/// past the pane: the name yields width, with an ellipsis.
#[test]
fn the_header_fits_the_width_whatever_the_name() {
    let name = "\u{7814}\u{7a76}\u{30ef}\u{30fc}\u{30ab}\u{30fc}".repeat(6);
    for direction in [
        AgentMessageDirection::ToSubagent,
        AgentMessageDirection::Question,
    ] {
        let view = AgentMessageView {
            subagent_name: &name,
            direction,
            text: "short",
            reply: None,
        };
        for width in [12u16, 20, 40] {
            for expanded in [false, true] {
                let lines = render(&view, &opts(width), expanded);
                for line in &lines {
                    let w: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
                    assert!(w <= width as usize, "{w} > {width}: {line:?}");
                }
                let head: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(head.contains('\u{2026}'), "{head}");
            }
        }
    }
}
