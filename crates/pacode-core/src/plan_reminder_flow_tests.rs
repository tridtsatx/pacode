//! End to end: a scripted main agent that stops with open plan items, through
//! a real core, store and turn loop.

use std::sync::Arc;

use pacode_types::{
    AgentId, Config, Event, Mode, Plan, PlanItem, PlanStatus, ToastLevel, TranscriptKind,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::rpc::harness::*;

const REMINDER: &str = "<system-reminder>You stopped with";

fn set_plan_step() -> Step {
    tool(
        "plan",
        json!({"action": "set", "items": [
            {"content": "wire it", "status": "active"},
            {"content": "test it"},
        ]}),
    )
}

fn update(item: &str, mut change: Value) -> Step {
    change["action"] = json!("update");
    change["item_id"] = json!(item);
    tool("plan", change)
}

/// Two open items set directly, for tests that are not about the plan tool.
fn open_plan(h: &Harness) {
    let item = |id: &str, status| PlanItem {
        id: id.into(),
        content: format!("step {id}"),
        status,
        progress: None,
    };
    h.session.set_plan(Plan {
        version: 1,
        items: vec![
            item("p1", PlanStatus::Done),
            item("p2", PlanStatus::Active),
            item("p3", PlanStatus::Pending),
        ],
    });
}

/// The reminder lines the client was shown.
fn notices(h: &Harness) -> Vec<String> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            Event::ItemAdded(item) => match item.kind {
                TranscriptKind::Notice {
                    level: ToastLevel::Info,
                    text,
                } if text.starts_with('↻') => Some(text),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Reminders the main agent's last request carried.
fn reminders_seen(h: &Harness) -> Vec<String> {
    let seen = h.provider.seen(false);
    let Some(last) = seen.last() else {
        return Vec::new();
    };
    last.match_indices(REMINDER)
        .map(|(at, _)| {
            let rest = &last[at..];
            let end = rest
                .find("</system-reminder>")
                .map_or(rest.len(), |end| end + "</system-reminder>".len());
            rest[..end].to_string()
        })
        .collect()
}

async fn go(h: &Harness, text: &str) {
    h.session.submit_user_message(text.into()).await.unwrap();
}

async fn settle(h: &Harness, turns: usize) {
    wait_for("the main agent to settle", || {
        h.main_turns_ended() >= turns && h.main_idle()
    })
    .await;
}

/// A stop that must not be reminded: one turn, then quiet.
async fn assert_no_reminder(h: &Harness) {
    settle(h, 1).await;
    assert_eq!(notices(h), Vec::<String>::new());
    assert!(reminders_seen(h).is_empty());
    assert_eq!(h.main_turns_ended(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_with_open_items_is_reminded_and_the_turn_goes_on() {
    let h = Harness::new().await;
    h.provider.script(
        false,
        vec![
            set_plan_step(),
            text("stopping here"),
            update("p1", json!({"status": "done"})),
            update("p2", json!({"status": "done"})),
            text("all done"),
        ],
    );
    go(&h, "go").await;
    settle(&h, 1).await;

    assert_eq!(notices(&h), vec!["↻ 2 plan items open — continuing (1/3)"]);
    let seen = h.provider.seen(false);
    assert!(
        seen[2].contains(
            "<system-reminder>You stopped with 2 open plan item(s):\n\
             - [active] wire it\n\
             - [ ] test it\n"
        ),
        "{seen:#?}"
    );
    assert!(seen[2].contains("(Reminder 1/3)</system-reminder>"));
    // The plan is finished: the second stop is final. One turn: the reminder
    // continued it, so a client waiting for `TurnEnded` saw all of it.
    assert_eq!(seen.len(), 5);
    assert_eq!(h.main_turns_ended(), 1);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reminder_awaits_progress_on_the_plan() {
    let h = Harness::new().await;
    h.provider.script(
        false,
        vec![set_plan_step(), text("stopping"), text("still stopping")],
    );
    go(&h, "go").await;
    settle(&h, 1).await;

    assert_eq!(notices(&h).len(), 1);
    assert_eq!(h.main_turns_ended(), 1);
    assert_eq!(h.provider.seen(false).len(), 3);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_budget_runs_out_after_max_reminders() {
    let h = Harness::new().await;
    let progress = |p: u8| update("p2", json!({"progress": p}));
    h.provider.script(
        false,
        vec![
            set_plan_step(),
            text("a"),
            progress(10),
            text("b"),
            progress(20),
            text("c"),
            progress(30),
            text("d"),
        ],
    );
    go(&h, "go").await;
    settle(&h, 1).await;

    assert_eq!(
        notices(&h),
        vec![
            "↻ 2 plan items open — continuing (1/3)",
            "↻ 2 plan items open — continuing (2/3)",
            "↻ 2 plan items open — continuing (3/3)",
        ]
    );
    let reminders = reminders_seen(&h);
    assert_eq!(reminders.len(), 3);
    assert!(reminders[2].ends_with("(Reminder 3/3)</system-reminder>"));
    assert_eq!(h.main_turns_ended(), 1);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_user_message_resets_the_budget() {
    let mut config = Config::default();
    config.session.plan_reminders_max = 1;
    let h = Harness::with_config(config).await;
    open_plan(&h);
    h.provider
        .script(false, vec![text("a"), text("b"), text("c"), text("d")]);
    go(&h, "go").await;
    settle(&h, 1).await;
    assert_eq!(notices(&h).len(), 1);

    go(&h, "next").await;
    settle(&h, 2).await;
    assert_eq!(
        notices(&h),
        vec![
            "↻ 2 plan items open — continuing (1/1)",
            "↻ 2 plan items open — continuing (1/1)",
        ]
    );
    assert_eq!(h.main().plan_reminders.lock().unwrap().sent, 1);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_mode_is_never_reminded() {
    let h = Harness::new().await;
    h.session.meta.write().unwrap().mode = Mode::Plan;
    open_plan(&h);
    h.provider.script(false, vec![text("here is the plan")]);
    go(&h, "plan it").await;
    assert_no_reminder(&h).await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_reminders_stay_silent() {
    let mut config = Config::default();
    config.session.plan_reminders = false;
    let h = Harness::with_config(config).await;
    open_plan(&h);
    h.provider.script(false, vec![text("stopping")]);
    go(&h, "go").await;
    assert_no_reminder(&h).await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_plan_is_not_reminded() {
    let h = Harness::new().await;
    h.provider.script(
        false,
        vec![
            set_plan_step(),
            update("p1", json!({"status": "done"})),
            update("p2", json!({"status": "cancelled"})),
            text("done"),
        ],
    );
    go(&h, "go").await;
    assert_no_reminder(&h).await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_to_the_user_ends_the_turn_for_good() {
    let h = Harness::new().await;
    open_plan(&h);
    h.provider.script(
        false,
        vec![text("Step 2 needs a choice.\nShould I keep the old API?")],
    );
    go(&h, "go").await;
    assert_no_reminder(&h).await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_turn_is_not_reminded() {
    let h = Harness::new().await;
    open_plan(&h);
    h.provider.script(false, vec![fail("provider down")]);
    go(&h, "go").await;
    assert_no_reminder(&h).await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_turn_is_not_reminded() {
    let h = Harness::new().await;
    open_plan(&h);
    h.provider.script(false, vec![tool("gate", json!({}))]);
    go(&h, "go").await;
    h.gate_entered().await;
    h.session.interrupt();
    assert_no_reminder(&h).await;
    h.gate.release.add_permits(1);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_preempted_by_the_user_is_not_reminded() {
    let h = Harness::new().await;
    open_plan(&h);
    h.provider.script(
        false,
        vec![
            tool("gate", json!({})),
            text("Which of the two do you want first?"),
        ],
    );
    go(&h, "go").await;
    h.gate_entered().await;
    go(&h, "actually, do this instead").await;
    h.gate.release.add_permits(1);
    settle(&h, 2).await;
    assert_eq!(notices(&h), Vec::<String>::new());
    assert!(reminders_seen(&h).is_empty());
    assert_eq!(h.main_turns_ended(), 2);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_background_task_will_wake_the_agent_instead() {
    let h = Harness::new().await;
    open_plan(&h);
    let cwd = h.session.meta().cwd;
    h.session
        .tasks
        .spawn(pacode_exec::TaskSpec::new(
            h.session.id.clone(),
            AgentId::main(),
            "sleep 30",
            cwd,
        ))
        .await
        .unwrap();
    h.provider
        .script(false, vec![text("waiting for the build")]);
    go(&h, "go").await;
    assert_no_reminder(&h).await;
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_subagent_will_wake_the_agent_instead() {
    let h = Harness::new().await;
    open_plan(&h);
    let parked = Arc::new(Semaphore::new(0));
    h.provider.script(
        false,
        vec![
            tool(
                "agent",
                json!({"action": "spawn", "name": "tests", "prompt": "work"}),
            ),
            text("spawned, waiting"),
        ],
    );
    h.provider
        .script(true, vec![gated(&parked, text("report"))]);
    go(&h, "go").await;
    assert_no_reminder(&h).await;
    parked.add_permits(1);
    h.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_turn_takes_no_reminder_budget_along() {
    let h = Harness::new().await;
    open_plan(&h);
    h.provider.script(
        false,
        vec![tool("gate", json!({})), text("Shall I pick the next step?")],
    );
    h.provider
        .script(true, vec![text("finished in the background")]);
    go(&h, "go").await;
    h.gate_entered().await;
    let detached = h.session.detach_main_turn().await.unwrap();
    h.gate.release.add_permits(1);
    wait_for("the detached report to wake the new main agent", || {
        h.main_turns_ended() >= 1 && h.main_idle()
    })
    .await;

    assert_eq!(notices(&h), Vec::<String>::new());
    assert!(h.provider.seen(true).iter().all(|r| !r.contains(REMINDER)));
    assert!(reminders_seen(&h).is_empty());
    let old = h.session.agent(&detached).expect("the detached agent");
    assert!(old.injections.is_empty());
    assert_eq!(h.main().plan_reminders.lock().unwrap().sent, 0);
    h.core.shutdown().await;
}
