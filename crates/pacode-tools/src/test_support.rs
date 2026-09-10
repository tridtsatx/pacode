//! In-memory scheduling state for hosts written in tests.
//!
//! `ToolHost` is the only door from a tool into the core, so every stub host in a
//! test has to answer the scheduling calls too. This keeps those answers real —
//! jobs and monitors are actually stored and listed back — without each test
//! rebuilding the same bookkeeping.

use std::sync::Mutex;

use pacode_types::{
    CronJob, CronJobId, CronSchedule, MonitorCondition, MonitorId, MonitorInfo, MonitorStatus,
};

use crate::ToolError;

#[derive(Default)]
pub struct ScheduleStub {
    jobs: Mutex<Vec<CronJob>>,
    monitors: Mutex<Vec<MonitorInfo>>,
}

impl ScheduleStub {
    pub fn add_cron_job(
        &self,
        name: String,
        schedule: CronSchedule,
        prompt: String,
    ) -> Result<CronJob, ToolError> {
        let now = pacode_types::time::now_ms();
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
        job.reschedule(now)
            .map_err(|e| ToolError::invalid(e.to_string()))?;
        self.jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(job.clone());
        Ok(job)
    }

    pub fn list_cron_jobs(&self) -> Vec<CronJob> {
        self.jobs
            .lock()
            .map(|j| j.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn remove_cron_job(&self, id: &CronJobId) -> Result<(), ToolError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        let before = jobs.len();
        jobs.retain(|j| &j.id != id);
        if jobs.len() == before {
            return Err(ToolError::invalid(format!("unknown cron job {id}")));
        }
        Ok(())
    }

    pub fn add_monitor(
        &self,
        label: String,
        condition: MonitorCondition,
        poll_interval_secs: Option<u64>,
    ) -> Result<MonitorInfo, ToolError> {
        let info = MonitorInfo {
            id: MonitorId::generate(),
            label,
            condition,
            poll_interval_secs: MonitorInfo::normalize_poll_interval(poll_interval_secs),
            started_at_ms: pacode_types::time::now_ms(),
            status: MonitorStatus::Watching,
            last_check_ms: None,
            fired_at_ms: None,
        };
        self.monitors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(info.clone());
        Ok(info)
    }

    pub fn list_monitors(&self) -> Vec<MonitorInfo> {
        self.monitors
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    pub fn stop_monitor(&self, id: &MonitorId) -> Result<(), ToolError> {
        let mut monitors = self.monitors.lock().unwrap_or_else(|p| p.into_inner());
        let monitor = monitors
            .iter_mut()
            .find(|m| &m.id == id)
            .ok_or_else(|| ToolError::invalid(format!("unknown monitor {id}")))?;
        monitor.status = MonitorStatus::Stopped;
        Ok(())
    }
}
