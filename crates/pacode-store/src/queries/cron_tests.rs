use pacode_types::{CronJob, CronJobId, CronSchedule, SessionId, SessionMeta};

use crate::Store;

async fn store_with_session() -> (Store, SessionId) {
    let store = Store::open_in_memory().expect("store");
    let session = SessionId::new("ses_cron");
    let meta = SessionMeta {
        id: session.clone(),
        name: Some("cron".into()),
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: pacode_types::ModelRoute::new("mock", "mock"),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
    };
    store.upsert_session(&meta).await.expect("session");
    (store, session)
}

fn job(id: &str, name: &str) -> CronJob {
    CronJob {
        id: CronJobId::new(id),
        name: name.to_string(),
        schedule: CronSchedule::every(300),
        prompt: "check the build".to_string(),
        enabled: true,
        created_at_ms: 1_000,
        last_run_ms: None,
        next_run_ms: Some(1_300_000),
        last_status: None,
    }
}

#[tokio::test]
async fn a_cron_job_round_trips_through_the_store() {
    let (store, session) = store_with_session().await;
    store
        .upsert_cron_job(&session, &job("cron_1", "nightly"))
        .await
        .expect("insert");

    let jobs = store.list_cron_jobs(&session).await.expect("list");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].name, "nightly");
    assert_eq!(jobs[0].schedule, CronSchedule::every(300));
    assert_eq!(jobs[0].next_run_ms, Some(1_300_000));
    assert!(jobs[0].enabled);
}

#[tokio::test]
async fn upserting_the_same_id_updates_rather_than_duplicates() {
    let (store, session) = store_with_session().await;
    store
        .upsert_cron_job(&session, &job("cron_1", "nightly"))
        .await
        .expect("insert");

    let mut edited = job("cron_1", "nightly");
    edited.enabled = false;
    edited.last_run_ms = Some(2_000);
    edited.last_status = Some("sent".to_string());
    edited.next_run_ms = None;
    store
        .upsert_cron_job(&session, &edited)
        .await
        .expect("update");

    let jobs = store.list_cron_jobs(&session).await.expect("list");
    assert_eq!(jobs.len(), 1);
    assert!(!jobs[0].enabled);
    assert_eq!(jobs[0].last_run_ms, Some(2_000));
    assert_eq!(jobs[0].last_status.as_deref(), Some("sent"));
    assert_eq!(jobs[0].next_run_ms, None);
}

#[tokio::test]
async fn deleting_reports_whether_a_row_was_there() {
    let (store, session) = store_with_session().await;
    store
        .upsert_cron_job(&session, &job("cron_1", "nightly"))
        .await
        .expect("insert");

    assert!(
        store
            .delete_cron_job(&CronJobId::new("cron_1"))
            .await
            .expect("delete")
    );
    assert!(
        !store
            .delete_cron_job(&CronJobId::new("cron_1"))
            .await
            .expect("delete again")
    );
    assert!(
        store
            .list_cron_jobs(&session)
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn long_names_and_prompts_are_capped_on_the_way_in() {
    let (store, session) = store_with_session().await;
    let mut big = job("cron_big", &"n".repeat(super::NAME_MAX_CHARS + 50));
    big.prompt = "p".repeat(super::PROMPT_MAX_CHARS + 500);
    store.upsert_cron_job(&session, &big).await.expect("insert");

    let jobs = store.list_cron_jobs(&session).await.expect("list");
    assert_eq!(jobs[0].name.chars().count(), super::NAME_MAX_CHARS);
    assert_eq!(jobs[0].prompt.chars().count(), super::PROMPT_MAX_CHARS);
}

#[tokio::test]
async fn jobs_of_another_session_are_not_listed() {
    let (store, session) = store_with_session().await;
    store
        .upsert_cron_job(&session, &job("cron_1", "mine"))
        .await
        .expect("insert");

    let other = SessionId::new("ses_other");
    assert!(store.list_cron_jobs(&other).await.expect("list").is_empty());
}
