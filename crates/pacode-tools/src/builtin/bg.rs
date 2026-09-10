//! `bg`: manage background tasks.

use async_trait::async_trait;
use pacode_types::TaskId;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "bg";

pub struct BgTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct BgInput {
    action: String,
    task_id: Option<String>,
    lines: Option<usize>,
    current: Option<u64>,
    total: Option<u64>,
    percent: Option<f64>,
    message: Option<String>,
}

#[async_trait]
impl Tool for BgTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Background tasks: `list` running/finished tasks, `status` of one, `tail` its \
         last lines, `kill` it, or report `progress` (current/total or percent) for \
         a task you are supervising. A finished task's completion is delivered to \
         you automatically as a message — continue working or end your turn rather \
         than polling."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["list", "status", "tail", "kill", "progress"]},
                "task_id": {"type": "string"},
                "lines": {"type": "integer", "minimum": 1, "default": 80},
                "current": {"type": "integer"},
                "total": {"type": "integer"},
                "percent": {"type": "number"},
                "message": {"type": "string"}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `list`: one line per task `<id> <status> <label> <duration> [<progress>]`;
    /// `status`: full TaskInfo lines; `tail`: last N lines; `kill`: `host.kill_task`;
    /// `progress`: `host.report_task_progress` with source Reported. Missing
    /// task_id where required → InvalidInput.
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<BgInput>(input)?;
        match args.action.as_str() {
            "list" => {
                let tasks = ctx.host.list_tasks();
                if tasks.is_empty() {
                    return Ok(ToolOutput::text("No background tasks.").with_title("bg list"));
                }
                let now_ms = pacode_types::now_ms();
                let mut lines = Vec::new();
                for task in &tasks {
                    let status_str = match task.status {
                        pacode_types::TaskStatus::Running => "running",
                        pacode_types::TaskStatus::Completed => "completed",
                        pacode_types::TaskStatus::Failed => "failed",
                        pacode_types::TaskStatus::Killed => "killed",
                    };
                    let dur_s = task.duration_ms(now_ms) / 1000;
                    let progress_str = task
                        .progress
                        .as_ref()
                        .and_then(|p| p.short_label())
                        .map(|l| format!(" [{l}]"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "{} {} {} {}s{}",
                        task.id, status_str, task.label, dur_s, progress_str
                    ));
                }
                let joined = lines.join("\n");
                let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
                let preview = format!("{} tasks", tasks.len());
                Ok(ToolOutput::text(content)
                    .with_title("bg list")
                    .with_preview(preview))
            }
            "status" => {
                let task_id_str = args
                    .task_id
                    .ok_or_else(|| ToolError::invalid("task_id is required for status"))?;
                let id = TaskId::new(task_id_str);
                let info = ctx
                    .host
                    .task_info(&id)
                    .ok_or_else(|| ToolError::invalid(format!("task not found: {id}")))?;
                let dur_s = info.duration_ms(pacode_types::now_ms()) / 1000;
                let mut out = format!(
                    "Task: {}\nLabel: {}\nCommand: {}\nStatus: {:?}\nDuration: {}s\nExit code: {}\nCwd: {}\nSpool: {}\nBytes: {}",
                    info.id,
                    info.label,
                    info.command,
                    info.status,
                    dur_s,
                    info.exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "none".to_string()),
                    info.cwd.display(),
                    info.output_path.display(),
                    info.output_bytes,
                );
                if let Some(l) = info.progress.as_ref().and_then(|p| p.short_label()) {
                    out.push_str(&format!("\nProgress: {l}"));
                }
                let content = cap_output(&out, accept_large_output, ctx.output_cap_chars);
                let preview = format!("{:?}", info.status);
                Ok(ToolOutput::text(content)
                    .with_title(format!("bg status {id}"))
                    .with_preview(preview))
            }
            "tail" => {
                let task_id_str = args
                    .task_id
                    .ok_or_else(|| ToolError::invalid("task_id is required for tail"))?;
                let id = TaskId::new(task_id_str);
                let lines = args.lines.unwrap_or(80);
                let tail_lines = ctx.host.task_tail(&id, lines).await?;
                let joined = tail_lines.join("\n");
                let content = cap_output(&joined, accept_large_output, ctx.output_cap_chars);
                let preview = tail_lines
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .cloned()
                    .unwrap_or_default();
                Ok(ToolOutput::text(content)
                    .with_title(format!("bg tail {id}"))
                    .with_preview(preview))
            }
            "kill" => {
                let task_id_str = args
                    .task_id
                    .ok_or_else(|| ToolError::invalid("task_id is required for kill"))?;
                let id = TaskId::new(task_id_str);
                ctx.host.kill_task(&id).await?;
                let text = format!("Task {id} killed.");
                Ok(ToolOutput::text(text)
                    .with_title(format!("bg kill {id}"))
                    .with_preview("killed"))
            }
            "progress" => {
                let task_id_str = args
                    .task_id
                    .ok_or_else(|| ToolError::invalid("task_id is required for progress"))?;
                let id = TaskId::new(task_id_str);
                let progress = pacode_types::TaskProgress {
                    current: args.current,
                    total: args.total,
                    percent: args.percent.map(|p| p as f32),
                    message: args.message,
                    source: pacode_types::ProgressSource::Reported,
                    updated_at_ms: pacode_types::now_ms(),
                }
                .normalize();
                ctx.host.report_task_progress(&id, progress)?;
                let text = format!("Reported progress for task {id}.");
                Ok(ToolOutput::text(text)
                    .with_title(format!("bg progress {id}"))
                    .with_preview("progress reported"))
            }
            other => Err(ToolError::invalid(format!("unknown action: {other}"))),
        }
    }
}
