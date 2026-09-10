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

use std::sync::{Arc, Mutex, Weak};

use pacode_types::time::now_ms;
use pacode_types::{
    CronJob, CronJobId, CronSchedule, Event, MonitorCondition, MonitorId, MonitorInfo,
    MonitorStatus,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

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
    /// Woken whenever the set of deadlines changes.
    changed: Notify,
    task: Mutex<Option<JoinHandle<()>>>,
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
            changed: Notify::new(),
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
        monitors.push(info.clone());
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
        drop(monitors);
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
    fn due_monitors(&self, now: u64) -> Vec<MonitorInfo> {
        self.monitors
            .lock()
            .map(|m| {
                m.iter()
                    .filter(|m| m.status.is_live())
                    .filter(|m| m.next_check_at_ms().is_none_or(|next| next <= now))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_check(&self, id: &MonitorId, now: u64, fired: bool) -> Option<MonitorInfo> {
        let mut monitors = self.monitors.lock().unwrap_or_else(|p| p.into_inner());
        let monitor = monitors.iter_mut().find(|m| &m.id == id)?;
        monitor.last_check_ms = Some(now);
        if fired {
            monitor.status = MonitorStatus::Fired;
            monitor.fired_at_ms = Some(now);
        }
        Some(monitor.clone())
    }

    fn record_failure(&self, id: &MonitorId, now: u64) -> Option<MonitorInfo> {
        let mut monitors = self.monitors.lock().unwrap_or_else(|p| p.into_inner());
        let monitor = monitors.iter_mut().find(|m| &m.id == id)?;
        monitor.last_check_ms = Some(now);
        monitor.status = MonitorStatus::Failed;
        Some(monitor.clone())
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

        match deadline {
            None => {
                // Nothing scheduled: park. No timer, no wakeups, no cost.
                sched.changed.notified().await;
            }
            Some(at) => {
                let wait = at.saturating_sub(now);
                if wait > 0 {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(wait)) => {}
                        _ = sched.changed.notified() => {
                            drop(sched);
                            continue;
                        }
                    }
                }
            }
        }
        drop(sched);

        let (Some(sched), Some(session)) = (scheduler.upgrade(), session.upgrade()) else {
            return;
        };
        fire_due(&sched, &session).await;
    }
}

async fn fire_due(scheduler: &Arc<Scheduler>, session: &Arc<Session>) {
    let now = now_ms();

    for job in scheduler.take_due_jobs(now) {
        let status = match session.submit_user_message(job.prompt.clone()).await {
            Ok(()) => "sent".to_string(),
            Err(e) => format!("failed: {e}"),
        };
        let updated = scheduler
            .record_job_status(&job.id, status)
            .unwrap_or(job.clone());
        if let Err(e) = session.store.upsert_cron_job(&session.id, &updated).await {
            log::warn!("failed to persist cron job {}: {e}", updated.id);
        }
        session.events.emit(Event::CronUpdated(updated));
    }

    for monitor in scheduler.due_monitors(now) {
        let outcome = monitor::check_condition(&monitor.condition, &session.meta().cwd).await;
        let updated = match outcome {
            Ok(true) => {
                let info = scheduler.record_check(&monitor.id, now, true);
                if let Some(info) = &info {
                    session.inject_monitor_fired(info);
                }
                info
            }
            Ok(false) => scheduler.record_check(&monitor.id, now, false),
            Err(e) => {
                log::warn!("monitor {} check failed: {e}", monitor.id);
                scheduler.record_failure(&monitor.id, now)
            }
        };
        if let Some(info) = updated {
            session.events.emit(Event::MonitorUpdated(info));
        }
    }
}
