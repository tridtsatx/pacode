use pacode_types::{Plan, PlanItem, PlanStatus};

use super::*;

fn item(id: &str, status: PlanStatus) -> PlanItem {
    PlanItem {
        id: id.to_string(),
        content: format!("content {id}"),
        status,
        progress: None,
    }
}

fn plan(items: Vec<PlanItem>) -> Plan {
    Plan { version: 1, items }
}

#[test]
fn activating_an_item_demotes_the_previous_active_one() {
    let mut plan = plan(vec![
        item("p1", PlanStatus::Done),
        item("p2", PlanStatus::Active),
        item("p3", PlanStatus::Pending),
    ]);
    plan.items[1].progress = Some(60);

    apply_update(&mut plan, "p3", Some("active"), Some(10)).expect("update");

    assert_eq!(plan.items[0].status, PlanStatus::Done);
    // The previously active item falls back to pending and loses its progress —
    // a pending item has nothing to show a percentage for.
    assert_eq!(plan.items[1].status, PlanStatus::Pending);
    assert_eq!(plan.items[1].progress, None);
    assert_eq!(plan.items[2].status, PlanStatus::Active);
    assert_eq!(plan.items[2].progress, Some(10));

    let rendered = render_plan(&plan);
    assert!(rendered.contains("[ ] p2"), "{rendered}");
    assert!(rendered.contains("[*] (10%) p3"), "{rendered}");
    assert_eq!(rendered.matches("[*]").count(), 1, "{rendered}");
}

#[test]
fn non_active_updates_leave_other_items_alone() {
    let mut plan = plan(vec![
        item("p1", PlanStatus::Active),
        item("p2", PlanStatus::Pending),
    ]);

    apply_update(&mut plan, "p2", Some("done"), None).expect("update");

    assert_eq!(plan.items[0].status, PlanStatus::Active);
    assert_eq!(plan.items[1].status, PlanStatus::Done);
}

#[test]
fn an_unknown_item_id_is_an_invalid_input() {
    let mut plan = plan(vec![item("p1", PlanStatus::Pending)]);

    let err = apply_update(&mut plan, "p9", Some("active"), None).expect_err("unknown id");
    assert!(format!("{err}").contains("plan item not found"), "{err}");
}

// ---------------------------------------------------------------------------
// Input bounding. A plan is re-rendered into the rail and read back on every
// `get`, so what lands in it is paid for repeatedly — and a cut that is not
// reported is indistinguishable from a successful write.
// ---------------------------------------------------------------------------

#[test]
fn an_item_within_the_limits_is_untouched() {
    let (kept, dropped) = bound_item("a short step".to_string(), MAX_SET_CHARS);
    assert_eq!(kept, "a short step");
    assert_eq!(dropped, 0);
}

#[test]
fn an_over_long_item_is_cut_to_the_per_item_cap() {
    let long = "x".repeat(MAX_ITEM_CHARS + 100);
    let (kept, dropped) = bound_item(long, MAX_SET_CHARS);
    assert_eq!(kept.chars().count(), MAX_ITEM_CHARS);
    assert_eq!(dropped, 100);
}

#[test]
fn the_payload_budget_bounds_the_items_after_the_first() {
    // The budget is the point: without it a plan of 20 max-length items would
    // be 10 000 characters re-sent on every step, and `get` would come back
    // cut by the hard output cap with a marker the model cannot act on.
    let (kept, dropped) = bound_item("y".repeat(MAX_ITEM_CHARS + 50), 10);
    assert_eq!(
        kept.chars().count(),
        10,
        "the budget is a ceiling, not a target: kept {}",
        kept.chars().count()
    );
    assert!(dropped > 0, "an exhausted budget must report the loss");
}

#[test]
fn an_exhausted_budget_keeps_nothing_and_still_reports_the_loss() {
    // The case the `+1` was meant to cover, and it works without it: `dropped` is
    // measured against the full text, so a fully clipped item says so.
    let (kept, dropped) = bound_item("z".repeat(100), 0);
    assert_eq!(kept.chars().count(), 0);
    assert_eq!(dropped, 100, "a wholly dropped item is still a dropped one");
}

#[test]
fn the_whole_plan_never_exceeds_the_payload_budget() {
    // End to end on the arithmetic: 20 items of MAX_ITEM_CHARS each against one
    // MAX_SET_CHARS budget must come to the budget, not to it plus a character
    // per item.
    let mut budget = MAX_SET_CHARS;
    let mut total = 0usize;
    for _ in 0..MAX_ITEMS {
        let (kept, _) = bound_item("w".repeat(MAX_ITEM_CHARS * 2), budget);
        total += kept.chars().count();
        budget = budget.saturating_sub(kept.chars().count());
    }
    assert_eq!(
        total, MAX_SET_CHARS,
        "the plan as a whole must fit the budget, not just each item"
    );
}

#[test]
fn the_cut_never_leaves_trailing_whitespace() {
    let mut text = "z".repeat(MAX_ITEM_CHARS - 2);
    text.push_str("   and then some more");
    let (kept, _) = bound_item(text, MAX_SET_CHARS);
    assert!(!kept.ends_with(' '), "kept: {kept:?}");
}

#[test]
fn nothing_dropped_produces_no_note() {
    assert!(truncation_note(0, 0).is_none());
}

#[test]
fn a_dropped_item_count_is_quantified() {
    let note = truncation_note(7, 0).expect("a dropped count is reported");
    assert!(note.contains("7 item(s) past the 20-item limit"), "{note}");
}

#[test]
fn dropped_characters_are_quantified() {
    let note = truncation_note(0, 42).expect("a dropped count is reported");
    assert!(note.contains("42 character(s)"), "{note}");
    assert!(note.contains("500-char per-item"), "{note}");
}

#[test]
fn both_kinds_of_loss_are_reported_together() {
    let note = truncation_note(3, 42).expect("both losses are reported");
    assert!(note.contains("3 item(s)"), "{note}");
    assert!(note.contains("42 character(s)"), "{note}");
}
