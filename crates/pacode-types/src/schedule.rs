//! Scheduled work DTOs: cron jobs (persisted) and monitors (session-scoped, in memory).
//!
//! Pure data + pure parsing: turning an expression into a next-run instant needs a
//! calendar, which lives in `pacode-core::schedule::cron`.

use serde::{Deserialize, Serialize};

use crate::ids::{CronJobId, MonitorId};
use crate::state::{
    ID_MAX_CHARS, LimitError, LimitUnit, NAME_MAX_CHARS, TEXT_MAX_CHARS, check_bytes, check_chars,
    limit_error,
};

/// Hard cap of cron jobs per session (`CoreError::TooManyCronJobs` above it).
pub const MAX_CRON_JOBS: usize = 64;
/// Hard cap of live (watching) monitors per session.
pub const MAX_MONITORS: usize = 32;
/// Monitors are polled no faster than this.
pub const MIN_MONITOR_POLL_SECS: u64 = 1;
/// Poll interval used when the caller does not name one.
pub const DEFAULT_MONITOR_POLL_SECS: u64 = 5;
/// And no slower than this: a monitor nobody checks is a monitor that never fires.
pub const MAX_MONITOR_POLL_SECS: u64 = 86_400;
/// The latest instant a schedule is ever asked to work from, `9999-12-31T23:59:59.999Z`.
/// Times come off the wire and out of the store, so the search the cron evaluator
/// does from them is bounded here rather than allowed to run past the end of the
/// millisecond range and wrap.
pub const MAX_TIMESTAMP_MS: u64 = 253_402_300_799_999;
/// Caps on the fields of a scheduled job and a monitor. Both are written by a
/// model or a client, persisted, and shown in a list, so each is held to what
/// that can survive.
pub const CRON_EXPR_MAX_CHARS: usize = 200;
pub const CRON_STATUS_MAX_CHARS: usize = 200;
pub const MONITOR_LABEL_MAX_CHARS: usize = 120;
pub const MONITOR_COMMAND_MAX_CHARS: usize = 4_000;
pub const MONITOR_PATTERN_MAX_CHARS: usize = 1_000;
pub const MONITOR_PATH_MAX_CHARS: usize = 4_000;

/// Milliseconds in a seconds count, saturating: an interval that large is a
/// mistyped number, and a run time in the far future is a better answer than a
/// wrapped one that fires immediately.
fn ms_from_secs(secs: u64) -> u64 {
    secs.saturating_mul(1_000)
}

/// When a cron job runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronSchedule {
    /// Fixed interval in seconds, counted from the previous run (min 1).
    Every { secs: u64 },
    /// A cron expression, evaluated in UTC (`* * * * *` = every minute).
    Cron { expr: String },
}

impl CronSchedule {
    pub fn every(secs: u64) -> Self {
        Self::Every { secs: secs.max(1) }
    }

    pub fn cron(expr: impl Into<String>) -> Self {
        Self::Cron { expr: expr.into() }
    }

    /// Parse the model/user form: `every 90s`, `every 5m`, `every 2h`, `every 1d`,
    /// `every 30` (seconds), `@every 5m`, or a cron expression with five or six fields.
    /// Returns `None` for an empty string, an interval without a number, or a
    /// candidate longer than [`CRON_EXPR_MAX_CHARS`] — the parser has no use for
    /// more text than a real expression takes.
    pub fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim();
        if text.is_empty() || text.chars().count() > CRON_EXPR_MAX_CHARS {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        let interval_body = lower
            .strip_prefix("@every")
            .or_else(|| lower.strip_prefix("every"))
            .map(str::trim_start)
            .filter(|body| !body.is_empty());
        if let Some(body) = interval_body {
            return parse_interval(body).map(Self::every);
        }
        if text.split_whitespace().count() >= 5 {
            return Some(Self::Cron {
                expr: text.to_string(),
            });
        }
        None
    }

    /// The fixed interval, when this is one.
    pub fn interval_secs(&self) -> Option<u64> {
        match self {
            Self::Every { secs } => Some(*secs),
            Self::Cron { .. } => None,
        }
    }

    /// The first run at or after `after_ms`, in Unix milliseconds.
    ///
    /// An interval counts from `after_ms` (which the caller sets to the previous
    /// run, or to now for a job that has never fired); an expression is evaluated
    /// in UTC. An expression that cannot be parsed or can never match is an error,
    /// never a panic and never an unbounded search. `after_ms` is clamped to
    /// [`MAX_TIMESTAMP_MS`] and every step saturates, so no value off the wire can
    /// push the search or the addition past the end of the millisecond range.
    pub fn next_run_ms(&self, after_ms: u64) -> Result<u64, crate::cron_expr::CronError> {
        let from = after_ms.min(MAX_TIMESTAMP_MS);
        match self {
            Self::Every { secs } => Ok(from.saturating_add(ms_from_secs((*secs).max(1)))),
            Self::Cron { expr } => crate::cron_expr::CronExpr::parse(expr)?.next_after_ms(from),
        }
    }

    /// Whether the expression is usable at all, without computing a run time.
    pub fn validate(&self) -> Result<(), crate::cron_expr::CronError> {
        match self {
            Self::Every { .. } => Ok(()),
            Self::Cron { expr } => crate::cron_expr::CronExpr::parse(expr).map(|_| ()),
        }
    }

    /// Whether the schedule text fits the caps of the scheduled-job DTOs, for a
    /// request or a persisted job that has not been parsed yet.
    pub fn check_size(&self) -> Result<(), LimitError> {
        match self {
            Self::Every { .. } => Ok(()),
            Self::Cron { expr } => check_chars("cron expression", expr, CRON_EXPR_MAX_CHARS),
        }
    }

    /// Wire/CLI form: `every 5m` or the expression itself.
    pub fn display(&self) -> String {
        match self {
            Self::Every { secs } => format!("every {}", format_interval(*secs)),
            Self::Cron { expr } => expr.clone(),
        }
    }
}

/// Unit suffix of an interval literal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IntervalUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

impl IntervalUnit {
    fn parse(ch: char) -> Option<Self> {
        match ch {
            's' => Some(Self::Seconds),
            'm' => Some(Self::Minutes),
            'h' => Some(Self::Hours),
            'd' => Some(Self::Days),
            _ => None,
        }
    }

    fn seconds(self) -> u64 {
        match self {
            Self::Seconds => 1,
            Self::Minutes => 60,
            Self::Hours => 3_600,
            Self::Days => 86_400,
        }
    }
}

/// `90s` / `5m` / `2h` / `1d` / bare seconds. Values below 1 clamp to 1.
fn parse_interval(raw: &str) -> Option<u64> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let (digits, unit) = {
        let last = text.chars().last()?;
        match IntervalUnit::parse(last) {
            Some(unit) => (text[..text.len() - 1].trim(), unit),
            None => (text, IntervalUnit::Seconds),
        }
    };
    let value: u64 = digits.parse().ok()?;
    Some(value.saturating_mul(unit.seconds()).max(1))
}

/// `90s`, `5m`, `2h`, `1d` — the shortest exact form.
fn format_interval(secs: u64) -> String {
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 && secs.is_multiple_of(3_600) {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// One scheduled prompt. Persisted in `cron_jobs`; survives a daemon restart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CronJob {
    pub id: CronJobId,
    pub name: String,
    pub schedule: CronSchedule,
    /// Injected as a user turn when the job fires.
    pub prompt: String,
    pub enabled: bool,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_ms: Option<u64>,
    /// Short outcome of the last fire: `sent`, `queued`, `failed: …`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
}

impl CronJob {
    /// Recompute `next_run_ms` from the last run (or `now_ms` when it never ran).
    /// A disabled job has no next run.
    pub fn reschedule(&mut self, now_ms: u64) -> Result<(), crate::cron_expr::CronError> {
        if !self.enabled {
            self.next_run_ms = None;
            return Ok(());
        }
        let from = self
            .last_run_ms
            .unwrap_or(now_ms)
            .max(now_ms.saturating_sub(1));
        self.next_run_ms = Some(self.schedule.next_run_ms(from)?);
        Ok(())
    }

    /// Milliseconds until the next run; `None` when disabled or not scheduled.
    pub fn due_in_ms(&self, now_ms: u64) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        Some(self.next_run_ms?.saturating_sub(now_ms))
    }

    /// One line for a list view.
    pub fn summary_line(&self) -> String {
        let state = if self.enabled { "on" } else { "off" };
        let name = &self.name;
        let next = self
            .next_run_ms
            .map(|ms| crate::time::format_duration_ms(ms.saturating_sub(crate::time::now_ms())))
            .unwrap_or_else(|| "—".to_string());
        let last = self.last_status.as_deref().unwrap_or("never run");
        format!("{} [{state}] {name} next in {next} last: {last}", self.id)
    }

    /// Whether the job fits the caps of the scheduled-job DTOs, for a request or
    /// a record that came back out of the store.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars("cron id", self.id.as_str(), ID_MAX_CHARS)?;
        check_chars("cron name", &self.name, NAME_MAX_CHARS)?;
        check_bytes("cron prompt", &self.prompt, TEXT_MAX_CHARS)?;
        check_chars(
            "cron last status",
            self.last_status.as_deref().unwrap_or(""),
            CRON_STATUS_MAX_CHARS,
        )?;
        self.schedule.check_size()
    }
}

/// What a monitor waits for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MonitorCondition {
    /// The command exits 0.
    CommandSucceeds { command: String },
    /// No process matches the pattern (`pgrep -f`).
    ProcessGone { pattern: String },
    /// The path exists.
    FileExists { path: String },
    /// The file contains a match for the regex `pattern`.
    FileMatches { path: String, pattern: String },
}

impl MonitorCondition {
    /// Parse `kind` + params as the `monitor` tool receives them.
    pub fn from_parts(
        kind: &str,
        command: Option<&str>,
        pattern: Option<&str>,
        path: Option<&str>,
    ) -> Option<Self> {
        match kind {
            "command_succeeds" => Some(Self::CommandSucceeds {
                command: command?.trim().to_string(),
            }),
            "process_gone" => Some(Self::ProcessGone {
                pattern: pattern?.trim().to_string(),
            }),
            "file_exists" => Some(Self::FileExists {
                path: path?.trim().to_string(),
            }),
            "file_matches" => Some(Self::FileMatches {
                path: path?.trim().to_string(),
                pattern: pattern?.trim().to_string(),
            }),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::CommandSucceeds { .. } => "command_succeeds",
            Self::ProcessGone { .. } => "process_gone",
            Self::FileExists { .. } => "file_exists",
            Self::FileMatches { .. } => "file_matches",
        }
    }

    /// One line shown in the rail and in the fired notice.
    pub fn describe(&self) -> String {
        match self {
            Self::CommandSucceeds { command } => format!("`{command}` exits 0"),
            Self::ProcessGone { pattern } => format!("no process matches {pattern}"),
            Self::FileExists { path } => format!("{path} exists"),
            Self::FileMatches { path, pattern } => format!("{path} matches {pattern}"),
        }
    }

    /// True when evaluating this condition needs a process.
    pub fn needs_exec(&self) -> bool {
        match self {
            Self::CommandSucceeds { .. } | Self::ProcessGone { .. } => true,
            Self::FileExists { .. } | Self::FileMatches { .. } => false,
        }
    }

    /// Whether the condition fits the caps of the monitor DTOs.
    pub fn check_size(&self) -> Result<(), LimitError> {
        match self {
            Self::CommandSucceeds { command } => {
                check_chars("monitor command", command, MONITOR_COMMAND_MAX_CHARS)
            }
            Self::ProcessGone { pattern } => {
                check_chars("monitor pattern", pattern, MONITOR_PATTERN_MAX_CHARS)
            }
            Self::FileExists { path } => check_chars("monitor path", path, MONITOR_PATH_MAX_CHARS),
            Self::FileMatches { path, pattern } => {
                check_chars("monitor path", path, MONITOR_PATH_MAX_CHARS)?;
                check_chars("monitor pattern", pattern, MONITOR_PATTERN_MAX_CHARS)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorStatus {
    /// Polled on every `poll_interval_secs`.
    Watching,
    /// The condition held; the owner was told and polling stopped.
    Fired,
    /// Stopped by the user or by the session ending.
    Stopped,
    /// The check itself broke (bad pattern, spawn failure); polling stopped.
    Failed,
}

impl MonitorStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Fired => "fired",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// Still counted against the live-monitor cap and still polled.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Watching)
    }

    pub fn is_terminal(self) -> bool {
        !self.is_live()
    }
}

/// One monitor. Session-scoped and in memory only: it dies with the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// Short display label, e.g. `build finished`.
    pub label: String,
    pub condition: MonitorCondition,
    pub poll_interval_secs: u64,
    pub started_at_ms: u64,
    pub status: MonitorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at_ms: Option<u64>,
}

impl MonitorInfo {
    /// When the next poll is due, in Unix milliseconds — the same absolute form
    /// as `CronJob::next_run_ms`, so the scheduler can compare the two directly.
    /// `None` once the monitor is terminal.
    pub fn next_check_at_ms(&self) -> Option<u64> {
        if !self.status.is_live() {
            return None;
        }
        // A monitor that has never been checked is due at once: waiting a full
        // interval before the first look would miss a condition that already holds.
        match self.last_check_ms {
            None => Some(self.started_at_ms),
            Some(last) => Some(last.saturating_add(ms_from_secs(self.poll_interval_secs))),
        }
    }

    /// Milliseconds until the next poll; `None` once the monitor is terminal.
    pub fn next_check_in_ms(&self, now_ms: u64) -> Option<u64> {
        Some(self.next_check_at_ms()?.saturating_sub(now_ms))
    }

    /// Clamp a requested interval to the floor and the default. An interval past
    /// [`MAX_MONITOR_POLL_SECS`] is a mistyped number: it would mean a monitor
    /// nobody expects to fire for a day, so it comes back as the ceiling.
    pub fn normalize_poll_interval(requested: Option<u64>) -> u64 {
        requested
            .filter(|secs| *secs > 0)
            .unwrap_or(DEFAULT_MONITOR_POLL_SECS)
            .clamp(MIN_MONITOR_POLL_SECS, MAX_MONITOR_POLL_SECS)
    }

    /// Whether the monitor fits the caps of the monitor DTOs.
    pub fn validate(&self) -> Result<(), LimitError> {
        check_chars("monitor id", self.id.as_str(), ID_MAX_CHARS)?;
        check_chars("monitor label", &self.label, MONITOR_LABEL_MAX_CHARS)?;
        if self.status.is_live() && self.poll_interval_secs > MAX_MONITOR_POLL_SECS {
            return Err(limit_error(
                "monitor poll interval",
                usize::try_from(self.poll_interval_secs).unwrap_or(usize::MAX),
                MAX_MONITOR_POLL_SECS as usize,
                LimitUnit::Seconds,
            ));
        }
        self.condition.check_size()
    }

    pub fn summary_line(&self) -> String {
        format!(
            "{} [{}] {} — {}",
            self.id,
            self.status.as_str(),
            self.label,
            self.condition.describe()
        )
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod schedule_tests;
