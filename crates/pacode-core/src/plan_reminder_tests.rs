use pacode_types::{PlanItem, PlanStatus};

use super::*;

fn item(content: &str, status: PlanStatus) -> PlanItem {
    PlanItem {
        id: format!("p-{content}"),
        content: content.into(),
        status,
        progress: None,
    }
}

#[test]
fn render_lists_open_items_with_the_budget() {
    let open = [
        item("wire the config", PlanStatus::Active),
        item("write the tests", PlanStatus::Pending),
    ];
    assert_eq!(
        render(&open, 1, 3),
        "<system-reminder>You stopped with 2 open plan item(s):\n\
         - [active] wire the config\n\
         - [ ] write the tests\n\
         Continue working on them, or mark them done/cancelled with the `plan` tool if they \
         are finished or no longer needed. (Reminder 1/3)</system-reminder>"
    );
}

#[test]
fn render_caps_item_count_and_length() {
    let mut open: Vec<PlanItem> = (0..MAX_LISTED + 4)
        .map(|i| item(&format!("step {i}"), PlanStatus::Pending))
        .collect();
    open[0].content = format!("multi\nline {}", "x".repeat(ITEM_CHARS * 3));
    let text = render(&open, 2, 3);

    assert!(text.starts_with(&format!(
        "<system-reminder>You stopped with {} open plan item(s):\n",
        MAX_LISTED + 4
    )));
    assert!(text.contains("- … and 4 more\n"), "{text}");
    assert!(!text.contains(&format!("step {MAX_LISTED}")), "{text}");
    let first = text.lines().nth(1).expect("the first item");
    assert!(first.starts_with("- [ ] multi line x"), "{first}");
    assert!(first.ends_with('…'), "{first}");
    assert_eq!(
        first.trim_start_matches("- [ ] ").chars().count(),
        ITEM_CHARS
    );
    // Bounded whatever the plan holds: the plan tool caps an item at 500 chars.
    let worst: Vec<PlanItem> = (0..20)
        .map(|_| item(&"y".repeat(500), PlanStatus::Pending))
        .collect();
    assert!(render(&worst, 3, 3).chars().count() < (MAX_LISTED + 4) * (ITEM_CHARS + 16));
}

#[test]
fn the_notice_line_names_count_and_attempt() {
    assert_eq!(
        notice_line(2, 1, 3),
        "↻ 2 plan items open — continuing (1/3)"
    );
    assert_eq!(
        notice_line(1, 3, 3),
        "↻ 1 plan item open — continuing (3/3)"
    );
}

#[test]
fn a_question_to_the_user_awaits_them() {
    for text in [
        "Done with the parser.\n\nShould I also update the docs?",
        "Two options: A or B. Which one do you prefer?",
        "Tests pass. Want me to push it to your fork?",
        "- Q: sqlite or postgres?",
        "> Question 2: keep the old flag?",
        "Listo. ¿Seguimos con la migración?",
        "移行を続けますか？",
        "Please confirm the schema change.",
        "Let me know which approach you want.",
        "1. Choose a name for the crate",
        "Tell me when the server is up.",
    ] {
        assert!(awaits_user(text), "{text:?}");
    }
}

#[test]
fn a_statement_or_a_code_tail_does_not_await_the_user() {
    for text in [
        "",
        "Implemented the parser; next I will wire it in.",
        "    let value = parse(input)?",
        "Optional fields end in `?`, e.g. label?",
        "Done.",
        "I chose postgres because it is already deployed.",
        "Should I?\nI will continue with the remaining steps now.",
    ] {
        assert!(!awaits_user(text), "{text:?}");
    }
}
