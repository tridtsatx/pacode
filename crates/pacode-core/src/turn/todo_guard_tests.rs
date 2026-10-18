use super::*;

fn item(id: &str, status: PlanStatus) -> PlanItem {
    PlanItem {
        id: id.to_string(),
        content: format!("do {id}"),
        status,
        progress: None,
    }
}

fn base(plan: &[PlanItem]) -> GuardInput<'_> {
    GuardInput {
        enabled: true,
        max_reminders: 2,
        reminders_sent: 0,
        mode: Mode::Build,
        is_main: true,
        interrupted: false,
        plan,
        last_text: "All done.",
        asked_question: false,
        last_reminded: None,
    }
}

#[test]
fn triggers_with_pending_and_active_items() {
    let plan = vec![
        item("p1", PlanStatus::Active),
        item("p2", PlanStatus::Pending),
        item("p3", PlanStatus::Done),
    ];
    let open = should_remind(&base(&plan)).expect("should remind");
    assert_eq!(open.len(), 2);
    assert_eq!(open[0].id, "p1");
    assert_eq!(open[1].id, "p2");
}

#[test]
fn silent_when_everything_is_closed() {
    let plan = vec![
        item("p1", PlanStatus::Done),
        item("p2", PlanStatus::Cancelled),
    ];
    assert!(should_remind(&base(&plan)).is_none());
    assert!(should_remind(&base(&[])).is_none());
}

#[test]
fn silent_in_plan_mode() {
    let plan = vec![item("p1", PlanStatus::Pending)];
    let mut input = base(&plan);
    input.mode = Mode::Plan;
    assert!(should_remind(&input).is_none());
}

#[test]
fn silent_after_interrupt_when_disabled_or_for_subagents() {
    let plan = vec![item("p1", PlanStatus::Pending)];
    let mut input = base(&plan);
    input.interrupted = true;
    assert!(should_remind(&input).is_none());
    let mut input = base(&plan);
    input.enabled = false;
    assert!(should_remind(&input).is_none());
    let mut input = base(&plan);
    input.is_main = false;
    assert!(should_remind(&input).is_none());
}

#[test]
fn question_with_same_open_set_is_not_repeated() {
    let plan = vec![item("p1", PlanStatus::Pending)];
    let prev = open_items(&plan);
    let mut input = base(&plan);
    input.last_text = "Which option do you prefer?";
    input.last_reminded = Some(&prev);
    assert!(should_remind(&input).is_none());

    // a tool-based question counts too
    let mut input = base(&plan);
    input.asked_question = true;
    input.last_reminded = Some(&prev);
    assert!(should_remind(&input).is_none());

    // first reminder is still sent even when the answer is a question
    let mut input = base(&plan);
    input.last_text = "Shall I continue?  ";
    assert!(should_remind(&input).is_some());

    // changed open set: remind again
    let changed = vec![
        item("p1", PlanStatus::Pending),
        item("p2", PlanStatus::Pending),
    ];
    let mut input = base(&changed);
    input.last_text = "Which one?";
    input.last_reminded = Some(&prev);
    assert!(should_remind(&input).is_some());

    // not a question: same set is reminded again
    let mut input = base(&plan);
    input.last_reminded = Some(&prev);
    assert!(should_remind(&input).is_some());
}

#[test]
fn respects_max_per_turn_and_resets_with_new_guard() {
    let plan = vec![item("p1", PlanStatus::Pending)];
    let mut input = base(&plan);
    input.reminders_sent = 1;
    assert!(should_remind(&input).is_some());
    input.reminders_sent = 2;
    assert!(should_remind(&input).is_none());
    input.max_reminders = 0;
    input.reminders_sent = 0;
    assert!(should_remind(&input).is_none());

    // a new user turn builds a fresh guard: counters start at zero
    let guard = TodoGuard::default();
    assert_eq!(guard.sent, 0);
    assert!(!guard.asked_question);
    assert!(guard.last_reminded.is_none());
}

#[test]
fn note_calls_detects_ask_question() {
    let mut guard = TodoGuard::default();
    guard.note_calls(&[]);
    assert!(!guard.asked_question);
    let call = (
        pacode_types::CallId::generate(),
        "ask_question".to_string(),
        serde_json::json!({}),
        None,
    );
    guard.note_calls(&[call]);
    assert!(guard.asked_question);
}

#[test]
fn reminder_text_is_deterministic() {
    let plan = vec![
        item("p1", PlanStatus::Active),
        item("p2", PlanStatus::Pending),
    ];
    let text = format_reminder(&open_items(&plan));
    assert_eq!(
        text,
        "You stopped, but these plan items are still open:\n\
         1. [in_progress] p1: do p1\n\
         2. [pending] p2: do p2\n\
         Either finish them, or mark each as done/cancelled with the plan tool and explain \
         why it is no longer needed. Do not stop with open items."
    );
    assert_eq!(text, format_reminder(&open_items(&plan)));
    assert_eq!(
        notice_text(2, 1, 2),
        "todo guard: 2 items open \u{2014} asked the agent to continue (1/2)"
    );
    assert!(notice_text(1, 1, 2).starts_with("todo guard: 1 item open"));
}
