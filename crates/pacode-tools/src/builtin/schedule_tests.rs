use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

#[tokio::test]
async fn cron_add_registers_a_job_and_reports_the_next_run() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    let out = CronTool
        .call(
            json!({
                "action": "add",
                "name": "nightly",
                "schedule": "every 30m",
                "prompt": "run the suite",
                "intent": "schedule the suite"
            }),
            &ctx,
        )
        .await
        .expect("add");

    assert!(out.content.contains("job_id: cron_"), "{}", out.content);
    assert!(
        out.content.contains("schedule: every 30m"),
        "{}",
        out.content
    );
    assert_eq!(host.schedule.list_cron_jobs().len(), 1);
}

#[tokio::test]
async fn cron_rejects_a_schedule_it_cannot_read() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    let err = CronTool
        .call(
            json!({
                "action": "add",
                "name": "broken",
                "schedule": "sometimes",
                "prompt": "x",
                "intent": "try a bad schedule"
            }),
            &ctx,
        )
        .await
        .expect_err("must be refused");
    assert!(format!("{err}").contains("unrecognised schedule"), "{err}");
    assert!(host.schedule.list_cron_jobs().is_empty());
}

#[tokio::test]
async fn cron_list_and_remove_round_trip() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    CronTool
        .call(
            json!({"action": "add", "name": "nightly", "schedule": "every 1h", "prompt": "x", "intent": "add"}),
            &ctx,
        )
        .await
        .expect("add");
    let id = host.schedule.list_cron_jobs()[0].id.clone();

    let listed = CronTool
        .call(json!({"action": "list", "intent": "list"}), &ctx)
        .await
        .expect("list");
    assert!(listed.content.contains("nightly"), "{}", listed.content);

    CronTool
        .call(
            json!({"action": "remove", "job_id": id.to_string(), "intent": "remove"}),
            &ctx,
        )
        .await
        .expect("remove");
    assert!(host.schedule.list_cron_jobs().is_empty());

    let empty = CronTool
        .call(json!({"action": "list", "intent": "list"}), &ctx)
        .await
        .expect("list");
    assert_eq!(empty.content, "no cron jobs");
}

#[tokio::test]
async fn cron_needs_every_field_of_an_add() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    for (args, missing) in [
        (
            json!({"action": "add", "schedule": "every 1h", "prompt": "x", "intent": "i"}),
            "name",
        ),
        (
            json!({"action": "add", "name": "n", "prompt": "x", "intent": "i"}),
            "schedule",
        ),
        (
            json!({"action": "add", "name": "n", "schedule": "every 1h", "intent": "i"}),
            "prompt",
        ),
    ] {
        let err = CronTool
            .call(args, &ctx)
            .await
            .expect_err("must be refused");
        assert!(
            format!("{err}").contains(missing),
            "{err} should mention {missing}"
        );
    }

    let err = CronTool
        .call(json!({"action": "dance", "intent": "i"}), &ctx)
        .await
        .expect_err("unknown action");
    assert!(format!("{err}").contains("unknown action"), "{err}");
}

#[tokio::test]
async fn monitor_watch_accepts_each_condition_and_needs_its_fields() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    for args in [
        json!({"action": "watch", "condition": "command_succeeds", "command": "test -f done", "intent": "i"}),
        json!({"action": "watch", "condition": "process_gone", "process": "dota2", "intent": "i"}),
        json!({"action": "watch", "condition": "file_exists", "path": "target/release/pacode", "intent": "i"}),
        json!({"action": "watch", "condition": "file_matches", "path": "build.log", "pattern": "Finished", "intent": "i"}),
    ] {
        MonitorTool.call(args, &ctx).await.expect("watch");
    }
    assert_eq!(host.schedule.list_monitors().len(), 4);

    let err = MonitorTool
        .call(
            json!({"action": "watch", "condition": "file_matches", "path": "x", "intent": "i"}),
            &ctx,
        )
        .await
        .expect_err("missing pattern");
    assert!(format!("{err}").contains("pattern"), "{err}");

    let err = MonitorTool
        .call(
            json!({"action": "watch", "condition": "telepathy", "intent": "i"}),
            &ctx,
        )
        .await
        .expect_err("unknown condition");
    assert!(format!("{err}").contains("unknown condition"), "{err}");
}

#[tokio::test]
async fn monitor_list_and_stop_round_trip() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    MonitorTool
        .call(
            json!({"action": "watch", "label": "build", "condition": "file_exists", "path": "done", "intent": "i"}),
            &ctx,
        )
        .await
        .expect("watch");
    let id = host.schedule.list_monitors()[0].id.clone();

    let listed = MonitorTool
        .call(json!({"action": "list", "intent": "i"}), &ctx)
        .await
        .expect("list");
    assert!(listed.content.contains("build"), "{}", listed.content);

    MonitorTool
        .call(
            json!({"action": "stop", "monitor_id": id.to_string(), "intent": "i"}),
            &ctx,
        )
        .await
        .expect("stop");
    assert!(
        host.schedule.list_monitors()[0].status.is_terminal(),
        "a stopped monitor must be terminal"
    );
}

#[tokio::test]
async fn long_values_from_the_model_are_capped() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host.clone());

    CronTool
        .call(
            json!({
                "action": "add",
                "name": "n".repeat(NAME_MAX_CHARS + 100),
                "schedule": "every 1h",
                "prompt": "p".repeat(PROMPT_MAX_CHARS + 100),
                "intent": "i"
            }),
            &ctx,
        )
        .await
        .expect("add");

    let job = host.schedule.list_cron_jobs()[0].clone();
    assert_eq!(job.name.chars().count(), NAME_MAX_CHARS);
    assert_eq!(job.prompt.chars().count(), PROMPT_MAX_CHARS);
}
