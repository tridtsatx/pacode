//! `cron` (scheduled prompts) and `monitor` (condition watchers).
//!
//! Both are how the model arranges to be woken later instead of blocking a turn on
//! a wait: `cron` sends a prompt on a schedule, `monitor` reports once a condition
//! it is watching becomes true.

use async_trait::async_trait;
use pacode_types::{CronJobId, CronSchedule, MonitorCondition, MonitorId, RiskLevel};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{parse_input, preview_body};
use crate::host::PermissionDraft;
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod schedule_tests;

pub const CRON_NAME: &str = "cron";
pub const MONITOR_NAME: &str = "monitor";

/// Model-facing descriptions; see `prompts/cron.md` and `prompts/monitor.md`
/// for the convention.
const CRON_DESCRIPTION: &str = include_str!("../prompts/cron.md");
const MONITOR_DESCRIPTION: &str = include_str!("../prompts/monitor.md");

/// Longest values accepted from the model, so a job or a monitor cannot carry an
/// unbounded string into the session or the store.
pub const NAME_MAX_CHARS: usize = 120;
pub const PROMPT_MAX_CHARS: usize = 8000;
pub const CONDITION_MAX_CHARS: usize = 2000;

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Baseline risk for an action that changes the session's future behaviour on
/// its own — a job that fires a prompt later, a watcher that runs a command on
/// its own. The gate answers `Ask` for it in Build mode instead of letting a
/// Control tool through unattended, which is why the draft is filed as `Exec`.
const SCHEDULE_RISK: RiskLevel = RiskLevel::Low;

async fn require_schedule_permission(
    ctx: &ToolCtx,
    tool: &str,
    title: String,
    detail: String,
    risk: RiskLevel,
) -> Result<(), ToolError> {
    let draft = PermissionDraft::new(
        ctx.agent.clone(),
        ctx.agent_name.clone(),
        ctx.call_id.clone(),
        title,
        detail,
        Some(risk),
    )
    .with_tool(tool, ToolKind::Exec);
    ctx.host
        .request_permission(draft)
        .await
        .into_result("user denied the request")
        .map(|_| ())
}

#[derive(Default)]
pub struct CronTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CronInput {
    action: String,
    name: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
    job_id: Option<String>,
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        CRON_NAME
    }

    fn description(&self) -> &str {
        CRON_DESCRIPTION.trim()
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["add", "list", "remove"]},
                "name": {"type": "string", "description": "Short label shown in the sidebar."},
                "schedule": {"type": "string", "description": "`every 30m` / `every 2h` / `0 3 * * *` (UTC)."},
                "prompt": {"type": "string", "description": "Sent to the session when the job fires."},
                "job_id": {"type": "string", "description": "For remove."}
            }
        })
    }

    fn can_exceed_output_cap(&self) -> bool {
        false
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _): (CronInput, bool) = parse_input(input)?;
        match args.action.as_str() {
            "add" => {
                let name = args
                    .name
                    .filter(|n| !n.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("add needs a name"))?;
                let schedule_text = args
                    .schedule
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("add needs a schedule"))?;
                let prompt = args
                    .prompt
                    .filter(|p| !p.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("add needs a prompt"))?;
                let schedule = CronSchedule::parse(&schedule_text).ok_or_else(|| {
                    ToolError::invalid(format!(
                        "unrecognised schedule {schedule_text:?}: use `every 30m` or a five-field cron expression"
                    ))
                })?;

                let name = cap(&name, NAME_MAX_CHARS);
                let prompt = cap(&prompt, PROMPT_MAX_CHARS);
                require_schedule_permission(
                    ctx,
                    CRON_NAME,
                    format!("cron add {name}"),
                    format!("runs on {}\n\n{prompt}", schedule.display()),
                    SCHEDULE_RISK,
                )
                .await?;

                let job = ctx.host.add_cron_job(name, schedule, prompt).await?;

                let next = job
                    .next_run_ms
                    .map(|ms| {
                        pacode_types::time::format_duration_ms(
                            ms.saturating_sub(pacode_types::time::now_ms()),
                        )
                    })
                    .unwrap_or_else(|| "never".to_string());
                let content = format!(
                    "job_id: {}\nname: {}\nschedule: {}\nnext run in {next}",
                    job.id,
                    job.name,
                    job.schedule.display()
                );
                let preview = preview_body(&content);
                Ok(ToolOutput::text(content)
                    .with_title(format!("cron add {}", job.name))
                    .with_preview(preview))
            }
            "list" => {
                let jobs = ctx.host.list_cron_jobs();
                if jobs.is_empty() {
                    return Ok(ToolOutput::text("no cron jobs".to_string())
                        .with_title("cron list")
                        .with_preview("no cron jobs"));
                }
                let lines: Vec<String> = jobs.iter().map(|j| j.summary_line()).collect();
                let joined = lines.join("\n");
                let preview = preview_body(&joined);
                Ok(ToolOutput::text(joined)
                    .with_title("cron list")
                    .with_preview(preview))
            }
            "remove" => {
                let id = args
                    .job_id
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("remove needs a job_id"))?;
                let id = CronJobId::new(id);
                let known = ctx
                    .host
                    .list_cron_jobs()
                    .into_iter()
                    .find(|job| job.id == id);
                let detail = match &known {
                    Some(job) => {
                        format!("stops the job `{}` ({})", job.name, job.schedule.display())
                    }
                    None => format!("stops job {id}"),
                };
                require_schedule_permission(
                    ctx,
                    CRON_NAME,
                    format!("cron remove {id}"),
                    detail,
                    SCHEDULE_RISK,
                )
                .await?;
                ctx.host.remove_cron_job(&id).await?;
                Ok(ToolOutput::text(format!("removed {id}"))
                    .with_title(format!("cron remove {id}"))
                    .with_preview(format!("removed {id}")))
            }
            other => Err(ToolError::invalid(format!(
                "unknown action {other:?}: use add, list or remove"
            ))),
        }
    }
}

#[derive(Default)]
pub struct MonitorTool {
    definition: DefinitionCache,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MonitorInput {
    action: String,
    label: Option<String>,
    condition: Option<String>,
    command: Option<String>,
    process: Option<String>,
    path: Option<String>,
    pattern: Option<String>,
    poll_interval_secs: Option<u64>,
    monitor_id: Option<String>,
}

#[async_trait]
impl Tool for MonitorTool {
    fn name(&self) -> &str {
        MONITOR_NAME
    }

    fn description(&self) -> &str {
        MONITOR_DESCRIPTION.trim()
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["watch", "list", "stop"]},
                "label": {"type": "string", "description": "Short label shown in the sidebar."},
                "condition": {
                    "type": "string",
                    "enum": ["command_succeeds", "process_gone", "file_exists", "file_matches"]
                },
                "command": {"type": "string", "description": "For command_succeeds."},
                "process": {"type": "string", "description": "For process_gone: the pattern to look for."},
                "path": {"type": "string", "description": "For file_exists and file_matches."},
                "pattern": {"type": "string", "description": "For file_matches: the substring to find."},
                "poll_interval_secs": {"type": "integer", "minimum": 1, "description": "How often to check; default 5."},
                "monitor_id": {"type": "string", "description": "For stop."}
            }
        })
    }

    fn can_exceed_output_cap(&self) -> bool {
        false
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _): (MonitorInput, bool) = parse_input(input)?;
        match args.action.as_str() {
            "watch" => {
                let condition = build_condition(&args)?;
                let label = args
                    .label
                    .filter(|l| !l.trim().is_empty())
                    .unwrap_or_else(|| condition.describe());
                let label = cap(&label, NAME_MAX_CHARS);
                let risk = match &condition {
                    MonitorCondition::CommandSucceeds { command } => {
                        pacode_risk::classify(command, &ctx.cwd).level
                    }
                    _ => SCHEDULE_RISK,
                };
                require_schedule_permission(
                    ctx,
                    MONITOR_NAME,
                    format!("monitor watch {label}"),
                    format!(
                        "checks {} every {}s until it is true",
                        condition.describe(),
                        pacode_types::MonitorInfo::normalize_poll_interval(args.poll_interval_secs)
                    ),
                    risk,
                )
                .await?;
                let info = ctx
                    .host
                    .add_monitor(label, condition, args.poll_interval_secs)?;
                let content = format!(
                    "monitor_id: {}\nlabel: {}\nwatching: {}\nchecking every {}s",
                    info.id,
                    info.label,
                    info.condition.describe(),
                    info.poll_interval_secs
                );
                let preview = preview_body(&content);
                Ok(ToolOutput::text(content)
                    .with_title(format!("monitor {}", info.label))
                    .with_preview(preview))
            }
            "list" => {
                let monitors = ctx.host.list_monitors();
                if monitors.is_empty() {
                    return Ok(ToolOutput::text("no monitors".to_string())
                        .with_title("monitor list")
                        .with_preview("no monitors"));
                }
                let lines: Vec<String> = monitors.iter().map(|m| m.summary_line()).collect();
                let joined = lines.join("\n");
                let preview = preview_body(&joined);
                Ok(ToolOutput::text(joined)
                    .with_title("monitor list")
                    .with_preview(preview))
            }
            "stop" => {
                let id = args
                    .monitor_id
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| ToolError::invalid("stop needs a monitor_id"))?;
                let id = MonitorId::new(id);
                let known = ctx
                    .host
                    .list_monitors()
                    .into_iter()
                    .find(|monitor| monitor.id == id);
                let detail = match &known {
                    Some(monitor) => format!(
                        "stops the watcher `{}` ({})",
                        monitor.label,
                        monitor.condition.describe()
                    ),
                    None => format!("stops watcher {id}"),
                };
                require_schedule_permission(
                    ctx,
                    MONITOR_NAME,
                    format!("monitor stop {id}"),
                    detail,
                    SCHEDULE_RISK,
                )
                .await?;
                ctx.host.stop_monitor(&id)?;
                Ok(ToolOutput::text(format!("stopped {id}"))
                    .with_title(format!("monitor stop {id}"))
                    .with_preview(format!("stopped {id}")))
            }
            other => Err(ToolError::invalid(format!(
                "unknown action {other:?}: use watch, list or stop"
            ))),
        }
    }
}

fn build_condition(args: &MonitorInput) -> Result<MonitorCondition, ToolError> {
    let kind = args
        .condition
        .as_deref()
        .ok_or_else(|| ToolError::invalid("watch needs a condition"))?;
    let need = |value: &Option<String>, field: &str| -> Result<String, ToolError> {
        value
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|v| cap(v, CONDITION_MAX_CHARS))
            .ok_or_else(|| ToolError::invalid(format!("{kind} needs {field}")))
    };
    match kind {
        "command_succeeds" => Ok(MonitorCondition::CommandSucceeds {
            command: need(&args.command, "command")?,
        }),
        "process_gone" => Ok(MonitorCondition::ProcessGone {
            pattern: need(&args.process, "process")?,
        }),
        "file_exists" => Ok(MonitorCondition::FileExists {
            path: need(&args.path, "path")?,
        }),
        "file_matches" => Ok(MonitorCondition::FileMatches {
            path: need(&args.path, "path")?,
            pattern: need(&args.pattern, "pattern")?,
        }),
        other => Err(ToolError::invalid(format!(
            "unknown condition {other:?}: use command_succeeds, process_gone, file_exists or file_matches"
        ))),
    }
}
