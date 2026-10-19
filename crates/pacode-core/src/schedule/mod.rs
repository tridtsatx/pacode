//! Scheduled prompts (cron jobs) and condition watchers (monitors).
//!
//! One task per session drives both. It sleeps until the nearest deadline and is
//! woken early when something changes; with no enabled job and no live monitor it
//! parks on a notification and consumes nothing at all, which is the repo's rule
//! that nothing may tick while the session is idle.
//!
//! Cron jobs are persisted (see `pacode_store::queries::cron`); monitors live only
//! as long as the session, and the task is aborted when the scheduler is dropped.

mod monitor;

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod schedule_tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use futures::stream::{FuturesUnordered, StreamExt};
use pacode_types::time::now_ms;
use pacode_types::{
    CronJob, CronJobId, CronSchedule, Event, MonitorCondition, MonitorId, MonitorInfo,
    MonitorStatus,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::session::Session;

pub use monitor::check_condition;

/// Most cron jobs one session may hold.
pub const MAX_CRON_JOBS: usize = 64;
/// Most live monitors one session may hold.
pub const MAX_MONITORS: usize = 32;
/// A monitor never polls faster than this.
pub const MIN_POLL_SECS: u64 = 1;
/// A condition check that has not answered by then counts as "not yet".
pub const CHECK_TIMEOUT_SECS: u64 = 20;

const TERMINAL_MONITOR_RETENTION: usize = MAX_MONITORS;

/// Why a scheduling request was refused.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("too many cron jobs: {MAX_CRON_JOBS} is the limit")]
    TooManyJobs,
    #[error("too many monitors: {MAX_MONITORS} is the limit")]
    TooManyMonitors,
    #[error("unknown cron job {0}")]
    UnknownJob(CronJobId),
    #[error("unknown monitor {0}")]
    UnknownMonitor(MonitorId),
    #[error("invalid schedule: {0}")]
    Schedule(#[from] pacode_types::cron_expr::CronError),
    #[error(transparent)]
    Store(#[from] pacode_store::StoreError),
}

/// Cron jobs and monitors of one session, plus the task that fires them.
pub struct Scheduler {
    jobs: Mutex<Vec<CronJob>>,
    monitors: Mutex<Vec<MonitorInfo>>,
    monitor_cancels: Mutex<HashMap<MonitorId, CancellationToken>>,
    changed: Arc<Notify>,
    task: Mutex<Option<JoinHandle<()>>>,
}

enum DueWork {
    Job(CronJob),
    Monitor(monitor::MonitorCheck),
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Monitors are session-scoped: nothing outlives the session that owns them.
        if let Ok(mut task) = self.task.lock()
            && let Some(handle) = task.take()
        {
            handle.abort();
        }
    }
}

impl Scheduler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            jobs: Mutex::new(Vec::new()),
            monitors: Mutex::new(Vec::new()),
            monitor_cancels: Mutex::new(HashMap::new()),
            changed: Arc::new(Notify::new()),
            task: Mutex::new(None),
        })
    }

    /// Start the driving task. `session` is weak so the scheduler never keeps the
    /// session alive; the task exits as soon as the session is gone.
    pub fn start(self: &Arc<Self>, session: Weak<Session>) {
        let me = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            run(me, session).await;
        });
        if let Ok(mut slot) = self.task.lock()
            && let Some(previous) = slot.replace(handle)
        {
            previous.abort();
        }
    }

    pub fn jobs(&self) -> Vec<CronJob> {
        self.jobs
            .lock()
            .map(|j| j.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    /// Install jobs loaded from the store, recomputing their next run.
    pub fn load_jobs(&self, mut jobs: Vec<CronJob>, now: u64) {
        jobs.truncate(MAX_CRON_JOBS);
        for job in &mut jobs {
            if let Err(e) = job.reschedule(now) {
                job.enabled = false;
                job.last_status = Some(format!("disabled: {e}"));
            }
        }
        if let Ok(mut slot) = self.jobs.lock() {
            *slot = jobs;
        }
        self.changed.notify_one();
    }

    pub fn add_job(
        &self,
        name: String,
        schedule: CronSchedule,
        prompt: String,
        now: u64,
    ) -> Result<CronJob, ScheduleError> {
        schedule.validate()?;
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.len() >= MAX_CRON_JOBS {
            return Err(ScheduleError::TooManyJobs);
        }
        let mut job = CronJob {
            id: CronJobId::generate(),
            name,
            schedule,
            prompt,
            enabled: true,
            created_at_ms: now,
            last_run_ms: None,
            next_run_ms: None,
            last_status: None,
        };
        job.reschedule(now)?;
        jobs.push(job.clone());
        drop(jobs);
        self.changed.notify_one();
        Ok(job)
    }

    pub fn remove_job(&self, id: &CronJobId) -> Result<(), ScheduleError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let before = jobs.len();
        jobs.retain(|j| &j.id != id);
        if jobs.len() == before {
            return Err(ScheduleError::UnknownJob(id.clone()));
        }
        drop(jobs);
        self.changed.notify_one();
        Ok(())
    }

    pub fn set_enabled(
        &self,
        id: &CronJobId,
        enabled: bool,
        now: u64,
    ) -> Result<CronJob, ScheduleError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let job = jobs
            .iter_mut()
            .find(|j| &j.id == id)
            .ok_or_else(|| ScheduleError::UnknownJob(id.clone()))?;
        job.enabled = enabled;
        job.reschedule(now)?;
        let updated = job.clone();
        drop(jobs);
        self.changed.notify_one();
        Ok(updated)
    }

    /// Mark a job as due right now, so the next wake fires it.
    pub fn run_now(&self, id: &CronJobId, now: u64) -> Result<CronJob, ScheduleError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let job = jobs
            .iter_mut()
            .find(|j| &j.id == id)
            .ok_or_else(|| ScheduleError::UnknownJob(id.clone()))?;
        job.next_run_ms = Some(now);
        let updated = job.clone();
        drop(jobs);
        self.changed.notify_one();
        Ok(updated)
    }

    pub fn add_monitor(
        &self,
        label: String,
        condition: MonitorCondition,
        poll_interval_secs: Option<u64>,
        now: u64,
    ) -> Result<MonitorInfo, ScheduleError> {
        let mut monitors = self.monitors.lock().unwrap_or_else(|p| p.into_inner());
        if monitors.iter().filter(|m| m.status.is_live()).count() >= MAX_MONITORS {
            return Err(ScheduleError::TooManyMonitors);
        }
        let info = MonitorInfo {
            id: MonitorId::generate(),
            label,
            condition,
            poll_interval_secs: MonitorInfo::normalize_poll_interval(poll_interval_secs)
                .max(MIN_POLL_SECS),
            started_at_ms: now,
            status: MonitorStatus::Watching,
            last_check_ms: None,
            fired_at_ms: None,
        };
        self.monitor_cancels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(info.id.clone(), CancellationToken::new());
        monitors.push(info.clone());
        monitor::retain_terminal(&mut monitors, None);
        drop(monitors);
        self.changed.notify_one();
        Ok(info)
    }

    pub fn stop_monitor(&self, id: &MonitorId) -> Result<MonitorInfo, ScheduleError> {
        let mut monitors = self.monitors.lock().unwrap_or_else(|p| p.into_inner());
        let monitor = monitors
            .iter_mut()
            .find(|m| &m.id == id)
            .ok_or_else(|| ScheduleError::UnknownMonitor(id.clone()))?;
        monitor.status = MonitorStatus::Stopped;
        let updated = monitor.clone();
        monitor::retain_terminal(&mut monitors, Some(id));
        drop(monitors);
        monitor::cancel(self, id);
        self.changed.notify_one();
        Ok(updated)
    }

    /// The nearest deadline across jobs and monitors, in Unix milliseconds.
    /// `None` means there is nothing to wait for and the task should park.
    pub fn next_deadline_ms(&self, _now: u64) -> Option<u64> {
        let jobs = self.jobs.lock().ok()?;
        let monitors = self.monitors.lock().ok()?;
        let job_next = jobs
            .iter()
            .filter(|j| j.enabled)
            .filter_map(|j| j.next_run_ms)
            .min();
        let monitor_next = monitors
            .iter()
            .filter(|m| m.status.is_live())
            .filter_map(|m| m.next_check_at_ms())
            .min();
        match (job_next, monitor_next) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Jobs whose time has come, marked as run. Returns them for firing.
    fn take_due_jobs(&self, now: u64) -> Vec<CronJob> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let mut due = Vec::new();
        for job in jobs.iter_mut() {
            if !job.enabled {
                continue;
            }
            let Some(next) = job.next_run_ms else {
                continue;
            };
            if next > now {
                continue;
            }
            job.last_run_ms = Some(now);
            if let Err(e) = job.reschedule(now) {
                job.enabled = false;
                job.last_status = Some(format!("disabled: {e}"));
                continue;
            }
            due.push(job.clone());
        }
        due
    }

    fn record_job_status(&self, id: &CronJobId, status: String) -> Option<CronJob> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let job = jobs.iter_mut().find(|j| &j.id == id)?;
        job.last_status = Some(status);
        Some(job.clone())
    }

    /// Monitors due for a check.
    fn due_monitors(&self, now: u64) -> Vec<monitor::MonitorCheck> {
        self.due_monitor_checks(now)
    }

    fn due_monitor_checks(&self, now: u64) -> Vec<monitor::MonitorCheck> {
        monitor::due_checks(self, now)
    }

    fn record_check(&self, id: &MonitorId, now: u64, fired: bool) -> Option<MonitorInfo> {
        monitor::record_check(self, id, now, fired)
    }

    fn record_failure(&self, id: &MonitorId, now: u64) -> Option<MonitorInfo> {
        monitor::record_failure(self, id, now)
    }
}

/// The driving loop: sleep to the nearest deadline, fire what is due, repeat.
async fn run(scheduler: Weak<Scheduler>, session: Weak<Session>) {
    loop {
        let Some(sched) = scheduler.upgrade() else {
            return;
        };
        let now = now_ms();
        let deadline = sched.next_deadline_ms(now);
        let changed = sched.changed.clone();
        drop(sched);

        match deadline {
            None => {
                changed.notified().await;
            }
            Some(at) => {
                let wait = at.saturating_sub(now);
                if wait > 0 {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(wait)) => {}
                        _ = changed.notified() => continue,
                    }
                }
            }
        }

        let (Some(sched), Some(session)) = (scheduler.upgrade(), session.upgrade()) else {
            return;
        };
        fire_due(&sched, &session).await;
    }
}

fn cron_fire_id(job: &CronJob, occurrence_ms: u64) -> String {
    format!("cron-{}-{occurrence_ms}", job.id)
}

async fn fire_due(scheduler: &Scheduler, session: &Arc<Session>) {
    let now = now_ms();
    let mut due = FuturesUnordered::new();

    for job in scheduler.take_due_jobs(now) {
        due.push(fire_work(scheduler, session, DueWork::Job(job)));
    }
    for check in scheduler.due_monitors(now) {
        due.push(fire_work(scheduler, session, DueWork::Monitor(check)));
    }

    while let Some(()) = due.next().await {}
}

async fn fire_work(scheduler: &Scheduler, session: &Arc<Session>, work: DueWork) {
    match work {
        DueWork::Job(job) => fire_job(scheduler, session, job).await,
        DueWork::Monitor(check) => monitor::fire(scheduler, session, check).await,
    }
}

async fn fire_job(scheduler: &Scheduler, session: &Arc<Session>, job: CronJob) {
    let mut admitted = job.clone();
    admitted.last_status = Some("queued".to_string());
    if let Err(error) = session.store.upsert_cron_job(&session.id, &admitted).await {
        let status = format!("failed to admit: {error}");
        if let Some(updated) = scheduler.record_job_status(&job.id, status) {
            session.events.emit(Event::CronUpdated(updated));
        }
        return;
    }

    let occurrence_ms = job.last_run_ms.unwrap_or_else(now_ms);
    let message_id = cron_fire_id(&job, occurrence_ms);
    let fingerprint = crate::core::user_message_fingerprint(&job.prompt, &[]);
    let status = match session
        .submit_user_message_with_images_idempotent(
            job.prompt.clone(),
            Vec::new(),
            message_id,
            fingerprint,
        )
        .await
    {
        Ok(()) => "sent".to_string(),
        Err(error) => format!("failed: {error}"),
    };
    let updated = scheduler.record_job_status(&job.id, status).unwrap_or(job);
    if let Err(error) = session.store.upsert_cron_job(&session.id, &updated).await {
        log::warn!("failed to persist cron job {}: {error}", updated.id);
    }
    session.events.emit(Event::CronUpdated(updated));
}
