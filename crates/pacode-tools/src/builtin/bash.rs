//! `bash`: run a shell command as a session task (spec §9).

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput, WaitOutcome};

#[cfg(test)]
#[path = "bash_tests.rs"]
mod bash_tests;
#[path = "ssh_multiplex.rs"]
mod ssh_multiplex;
#[cfg(test)]
#[path = "ssh_multiplex_tests.rs"]
mod ssh_multiplex_tests;

pub const NAME: &str = "bash";

/// How long a spawn may stay pending before the row shows "waiting for a process slot".
const SLOT_WAIT_NOTICE: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct BashTool {
    definition: DefinitionCache,
}

/// `pub(crate)` so `batch` can reuse the validated shape for its bash ops.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct BashInput {
    pub(crate) command: String,
    pub(crate) background: bool,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) label: Option<String>,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        include_str!("../prompts/bash.md").trim()
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string", "description": "Command line for `sh -c`."},
                "background": {"type": "boolean", "default": false, "description": "Return a task id immediately instead of waiting."},
                "timeout_secs": {"type": "integer", "minimum": 1, "description": "Kill the command after this many seconds (default: none for background, 3600 otherwise)."},
                "label": {"type": "string", "description": "Short label for the task list, e.g. `cargo test`."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }

    /// A plain read (`pacode_risk::is_side_effect_free`) that the risk
    /// classifier finds `Safe`, which the permission matrix allows in every
    /// mode without a prompt. Anything else keeps its place in the sequence,
    /// so a command that may write or ask stays ordered against its neighbours.
    ///
    /// Stricter than the Plan-mode allowlist (`is_read_only`) on purpose:
    /// that one also admits `sed`/`awk` scripts, `xargs`, `env` and tool
    /// versions by reading their arguments, which is fine for "leaves the
    /// project as it was" but not for "may run while another call runs" — a
    /// miss there is a silent race, not a refused command.
    fn runs_concurrently(&self, input: &Value, cwd: &std::path::Path) -> bool {
        let Some(command) = input.get("command").and_then(Value::as_str) else {
            return false;
        };
        pacode_risk::is_side_effect_free(command)
            && pacode_risk::classify(command, cwd).level == pacode_risk::RiskLevel::Safe
    }

    /// 1. `pacode_risk::classify(command, cwd)`; in `Mode::Plan` the command must also
    ///    satisfy `pacode_risk::is_read_only`, else `ToolError::Denied` without prompting.
    /// 2. `ctx.require_permission("Bash: <first 60 chars>", command + risk summary, Some(risk))`
    ///    (the host applies the matrix; Safe returns Allow without asking).
    /// 3. `host.spawn_task(TaskSpec{...})`; `background: true` → return
    ///    `{task_id, status: running}` immediately, title `Bash <label>`,
    ///    `ToolOutput.task = Some(id)`.
    /// 4. Foreground: `host.wait_task(id, yield_after, false)`; `Finished` → output from
    ///    `host.task_output` (head+tail, capped) + exit code line; `Timeout` → the task
    ///    keeps running: output `Command still running after Ns; moved to background as
    ///    task <id>. Latest output:\n<tail>`, `ToolOutput.task = Some(id)`.
    ///    The wait itself hands the task to the background, so its end arrives as
    ///    `<task_finished>`; a `Finished` wait means this result is the only report.
    /// `yield_after` comes from `ctx` (`ToolCtx::exec_yield_after`, added below).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large_output) = parse_input::<BashInput>(input)?;
        if args.command.trim().is_empty() {
            return Err(ToolError::invalid("command cannot be empty"));
        }

        let assessment = pacode_risk::classify(&args.command, &ctx.cwd);
        if ctx.mode == pacode_types::Mode::Plan && !pacode_risk::is_read_only(&args.command) {
            return Err(ToolError::Denied(
                "command is not read-only in Plan mode".to_string(),
            ));
        }

        let first_60: String = args.command.chars().take(60).collect();
        let perm_title = format!("Bash: {first_60}");
        let detail = if let Some(summary) = assessment.summary() {
            format!("{}\n\nRisk: {summary}", args.command)
        } else {
            args.command.clone()
        };
        ctx.require_permission(perm_title, detail, Some(assessment.level))
            .await?;

        // A pure pipeline of supported text utilities (`wc -l f | sort -n`) runs in-process:
        // no fork, same output shape. Anything else, or any failure, takes the normal path.
        if !args.background
            && let Some(out) = super::utils::try_in_process(&args.command, &ctx.cwd).await
        {
            let (content, preview) =
                super::utils::render_for_bash(&out, accept_large_output, ctx.output_cap_chars);
            let label = args
                .label
                .clone()
                .unwrap_or_else(|| pacode_types::task_label_from_command(&args.command));
            let mut result = ToolOutput::text(content)
                .with_title(format!("Bash {label} [in-process]"))
                .with_preview(preview);
            result.is_error = out.code != 0;
            return Ok(result);
        }

        run_authorized(args, accept_large_output, ctx, Some(ctx.call_id.clone())).await
    }
}

/// Spawn/wait/output — everything `call` does after classification and the
/// permission gate. `batch` reaches this for ops it has already checked with
/// `pacode_risk::is_read_only` and covered with its combined permission request,
/// so the command must already be authorized. `live_row`: the call whose
/// transcript row draws a foreground run's output as it comes; `batch` passes
/// its own row for a lone shell op and `None` for several, which run side by
/// side under that one row.
pub(crate) async fn run_authorized(
    args: BashInput,
    accept_large_output: bool,
    ctx: &ToolCtx,
    live_row: Option<pacode_types::CallId>,
) -> Result<ToolOutput, ToolError> {
    ssh_multiplex::check_and_warn(&args.command, ctx.host.as_ref()).await;

    let label = args
        .label
        .clone()
        .unwrap_or_else(|| pacode_types::task_label_from_command(&args.command));
    let tool_title = format!("Bash {label}");

    let timeout = if args.background {
        None
    } else {
        args.timeout_secs
            .map(Duration::from_secs)
            .or(Some(ctx.exec_default_timeout))
    };

    let mut spec = pacode_exec::TaskSpec::new(
        ctx.session.clone(),
        ctx.agent.clone(),
        args.command.clone(),
        ctx.cwd.clone(),
    );
    spec.label = args.label.clone();
    spec.background = args.background;
    spec.timeout = timeout;
    if !args.background {
        spec.call = live_row;
        // An interrupted call drops its guard: a plain read is killed then,
        // anything else is handed to the background and announced. Decided
        // here, with the spec, so it holds from the moment the task exists.
        spec.kill_if_abandoned = pacode_risk::is_side_effect_free(&args.command);
    }

    // Cancelling the call also abandons a wait in the exec admission queue.
    spec.cancel = Some(ctx.cancel.clone());

    // When all process slots are busy `spawn_task` queues (FIFO). A spawn that is still
    // pending after `SLOT_WAIT_NOTICE` is most likely queued: say so in the live row.
    let spawn = ctx.host.spawn_task(spec);
    tokio::pin!(spawn);
    let spawned = tokio::select! {
        r = &mut spawn => r,
        () = tokio::time::sleep(SLOT_WAIT_NOTICE) => {
            ctx.host.emit_preview(&ctx.call_id, "waiting for a process slot...".to_string());
            spawn.await
        }
    };
    let spawned = match spawned {
        Ok(spawned) => spawned,
        Err(_) if ctx.cancel.is_cancelled() => return Err(ToolError::Cancelled),
        Err(e) => return Err(e),
    };
    let id = spawned.id;

    if args.background {
        let content = format!("task_id: {id}\nstatus: running\nTask started in background.");
        let preview = format!("task {id} running in background");
        return Ok(ToolOutput::text(content)
            .with_title(tool_title)
            .with_preview(preview)
            .with_task(id));
    }

    // Held until the result is in hand; an interrupted call drops it, which
    // backgrounds the command (its end is announced, without waking the agent
    // after a user interrupt), or kills a plain read nobody needs any more.
    // An end the call never delivered is announced as well.
    let foreground = spawned.foreground;
    let result = foreground_result(&id, accept_large_output, ctx, tool_title).await;
    if let Some(guard) = foreground
        && !matches!(result, Err(ToolError::Cancelled))
    {
        guard.delivered();
    }
    result
}

/// Wait for a foreground task and turn its end, or the hand-off to the
/// background, into the call's result.
async fn foreground_result(
    id: &pacode_types::TaskId,
    accept_large_output: bool,
    ctx: &ToolCtx,
    tool_title: String,
) -> Result<ToolOutput, ToolError> {
    let id = id.clone();
    let outcome = ctx.host.wait_task(&id, ctx.exec_yield_after, false).await;
    match outcome {
        WaitOutcome::Finished => {
            let info = ctx.host.task_info(&id);
            let exit_code = info.as_ref().and_then(|i| i.exit_code).unwrap_or(0);
            let total_bytes = info.map_or(0, |i| i.output_bytes);
            // Head+tail, not a tail: the first error of a long build is
            // usually the one that matters. `accept_large_output` lifts the
            // char cap; the exec buffer's own head/tail caps still bound it.
            let max_chars = if accept_large_output {
                usize::MAX
            } else {
                ctx.output_cap_chars
            };
            let mut content = ctx.host.task_output(&id, max_chars).await?;
            let preview = preview_body(&content);
            // Past the exec buffer's 80 KB its own `bytes omitted` marker sits
            // in the middle the char cap cuts away, so the real size is named
            // on the last line whenever the model is not seeing all of it.
            let shown_all = total_bytes <= content.len() as u64;
            if !content.ends_with('\n') {
                content.push('\n');
            }
            if shown_all {
                content.push_str(&format!("[exit code {exit_code}]"));
            } else {
                content.push_str(&format!(
                    "[exit code {exit_code}, {total_bytes} bytes total]"
                ));
            }

            let mut out = ToolOutput::text(content)
                .with_title(tool_title)
                .with_preview(preview);
            if exit_code != 0 {
                out.is_error = true;
            }
            Ok(out)
        }
        WaitOutcome::Timeout | WaitOutcome::Progress => {
            let tail_lines = ctx.host.task_tail(&id, 40).await.unwrap_or_default();
            // 40 lines can still be the whole 64 KB tail buffer (progress
            // bars redraw with `\r`, never `\n`), so the cap applies here too.
            let tail_joined = cap_output(
                &tail_lines.join("\n"),
                accept_large_output,
                ctx.output_cap_chars,
            );
            let secs = if ctx.exec_yield_after.subsec_millis() == 0 {
                format!("{}s", ctx.exec_yield_after.as_secs())
            } else {
                format!("{:.1}s", ctx.exec_yield_after.as_secs_f32())
            };
            let content = format!(
                "Command still running after {secs}; moved to background as task {id}. Latest output:\n{tail_joined}"
            );
            let preview = if tail_joined.trim().is_empty() {
                format!("task {id} moved to background")
            } else {
                preview_body(&tail_joined)
            };

            Ok(ToolOutput::text(content)
                .with_title(tool_title)
                .with_preview(preview)
                .with_task(id))
        }
        WaitOutcome::Cancelled => Err(ToolError::Cancelled),
    }
}
