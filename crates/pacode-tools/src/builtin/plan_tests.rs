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
