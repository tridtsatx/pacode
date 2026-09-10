//! User hooks (`[hooks]` in the config): shell commands fired on tool and
//! lifecycle events, Claude-Code-style.
//!
//! Each matching rule runs `sh -c <command>` with context in `PACODE_HOOK_*`
//! env vars and a JSON payload on stdin
//! (`{event, session_id, cwd, tool?, input?, output?}`):
//!
//! - `pre_tool_use` — before a tool call. Exit code 2 blocks the call and the
//!   hook's stderr goes back to the model as the tool error; any other nonzero
//!   exit only warns. Timeout or spawn failure is fail-open (allow + notice):
//!   a hanging guard is worse than a missed one.
//! - `post_tool_use` — after the call. A nonzero/timed-out result surfaces its
//!   stderr as a notice; it never blocks.
//! - `session_start`/`session_end`/`notification` — fire-and-forget.
//!
//! Hook failures never abort a turn: everything degrades to a transcript
//! notice or a log line.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use pacode_types::{AgentId, Event, HookRule, ToastLevel, TranscriptItem, TranscriptKind};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::agent::Agent;
use crate::session::Session;

/// Event names sent to hooks (`PACODE_HOOK_EVENT` and the `event` stdin field).
pub const PRE_TOOL_USE: &str = "pre_tool_use";
pub const POST_TOOL_USE: &str = "post_tool_use";
pub const SESSION_START: &str = "session_start";
pub const SESSION_END: &str = "session_end";
pub const NOTIFICATION: &str = "notification";

/// A hook's stderr beyond this is truncated before it reaches the model or a
/// notice (spec §6.4: everything model-visible is capped).
const STDERR_CAP: usize = 4_000;

/// The decision `pre_tool_use` hooks reached for one tool call.
pub enum PreToolDecision {
    Allow,
    /// A hook exited with code 2; the text goes to the model as the tool error.
    Block(String),
}

/// Per-call context handed to a hook process.
struct HookCtx {
    event: &'static str,
    tool: Option<String>,
    session_id: String,
    cwd: PathBuf,
    input: Option<serde_json::Value>,
    output: Option<serde_json::Value>,
}

/// How a single hook process ended.
enum HookRun {
    Exited {
        code: i32,
        stderr: String,
    },
    /// Killed after `timeout_secs` (`kill_on_drop` does the kill).
    TimedOut {
        timeout_secs: u64,
    },
    /// Spawn or wait failure — the hook never really ran.
    Failed(String),
}

/// Rules of `rules` whose matcher accepts `tool`. `None` — lifecycle events
/// carry no tool name — only satisfies catch-all matchers, matched against the
/// empty string.
fn matching<'a>(rules: &'a [HookRule], tool: Option<&str>) -> Vec<&'a HookRule> {
    let subject = tool.unwrap_or("");
    rules
        .iter()
        .filter(|rule| matcher_matches(&rule.matcher, subject))
        .collect()
}

/// Empty or `.*` matchers catch everything; anything else is a regex. An
/// invalid regex matches nothing (fail-open) and warns once per evaluation.
fn matcher_matches(matcher: &str, subject: &str) -> bool {
    let m = matcher.trim();
    if m.is_empty() || m == ".*" {
        return true;
    }
    match regex::Regex::new(m) {
        Ok(re) => re.is_match(subject),
        Err(e) => {
            log::warn!("invalid [hooks] matcher {m:?}: {e}");
            false
        }
    }
}

/// The JSON document piped to the hook's stdin.
fn stdin_payload(ctx: &HookCtx) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "event": ctx.event,
        "session_id": ctx.session_id,
        "cwd": ctx.cwd,
    });
    if let Some(tool) = &ctx.tool {
        payload["tool"] = serde_json::Value::String(tool.clone());
    }
    if let Some(input) = &ctx.input {
        payload["input"] = input.clone();
    }
    if let Some(output) = &ctx.output {
        payload["output"] = output.clone();
    }
    payload
}

/// Spawn one rule and wait for it, capped at `timeout_secs` (minimum 1s so a
/// `0` cannot mean "never run"). Stdout is drained and ignored.
async fn run_rule(rule: &HookRule, ctx: &HookCtx) -> HookRun {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&rule.command)
        .current_dir(&ctx.cwd)
        .env("PACODE_HOOK_EVENT", ctx.event)
        .env("PACODE_HOOK_SESSION", &ctx.session_id)
        .env("PACODE_HOOK_CWD", &ctx.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(tool) = &ctx.tool {
        cmd.env("PACODE_HOOK_TOOL", tool);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return HookRun::Failed(e.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that never reads stdin sees EPIPE on our side — the write
        // result is deliberately ignored.
        let _ = stdin
            .write_all(stdin_payload(ctx).to_string().as_bytes())
            .await;
    }
    let timeout_secs = rule.timeout_secs.max(1);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(Ok(out)) => HookRun::Exited {
            code: out.status.code().unwrap_or(-1),
            stderr: pacode_types::truncate_head_tail(
                String::from_utf8_lossy(&out.stderr).trim(),
                STDERR_CAP,
            ),
        },
        Ok(Err(e)) => HookRun::Failed(e.to_string()),
        Err(_) => HookRun::TimedOut { timeout_secs },
    }
}

/// One-line description of a non-OK hook result, for notices and logs.
fn describe_outcome(event: &str, command: &str, outcome: &HookRun) -> Option<String> {
    match outcome {
        HookRun::Exited { code: 0, .. } => None,
        HookRun::Exited { code, stderr } if stderr.is_empty() => {
            Some(format!("{event} hook `{command}` exited {code}"))
        }
        HookRun::Exited { code, stderr } => {
            Some(format!("{event} hook `{command}` exited {code}: {stderr}"))
        }
        HookRun::TimedOut { timeout_secs } => Some(format!(
            "{event} hook `{command}` timed out after {timeout_secs}s"
        )),
        HookRun::Failed(e) => Some(format!("{event} hook `{command}` failed to run: {e}")),
    }
}

/// Surface a hook problem as a transcript notice; hook failures never abort a
/// turn, so a warn item is as loud as they get.
fn warn_notice(session: &Session, agent: &Agent, text: String) {
    let seq = agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .next_seq();
    let item = TranscriptItem {
        seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: TranscriptKind::Notice {
            level: ToastLevel::Warn,
            text,
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemAdded(item));
}

/// Run the matching `pre_tool_use` rules; split from [`pre_tool_use`] so tests
/// need no `Session`. The first exit-2 wins; every other failure only warns.
async fn eval_pre(rules: &[HookRule], tool: &str, ctx: &HookCtx) -> (PreToolDecision, Vec<String>) {
    let mut warnings = Vec::new();
    for rule in matching(rules, Some(tool)) {
        let outcome = run_rule(rule, ctx).await;
        if let HookRun::Exited { code: 2, stderr } = &outcome {
            let reason = if stderr.is_empty() {
                format!(
                    "{PRE_TOOL_USE} hook `{}` blocked the call (exit 2)",
                    rule.command
                )
            } else {
                stderr.clone()
            };
            return (PreToolDecision::Block(reason), warnings);
        }
        if let Some(text) = describe_outcome(PRE_TOOL_USE, &rule.command, &outcome) {
            warnings.push(format!("{text}; call allowed"));
        }
    }
    (PreToolDecision::Allow, warnings)
}

/// Run the matching `post_tool_use` rules; every failure becomes a warning.
async fn eval_post(rules: &[HookRule], tool: &str, ctx: &HookCtx) -> Vec<String> {
    let mut warnings = Vec::new();
    for rule in matching(rules, Some(tool)) {
        let outcome = run_rule(rule, ctx).await;
        if let Some(text) = describe_outcome(POST_TOOL_USE, &rule.command, &outcome) {
            warnings.push(text);
        }
    }
    warnings
}

/// `pre_tool_use` hooks for one tool call. Empty/absent config: zero overhead.
pub async fn pre_tool_use(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    tool: &str,
    input: &serde_json::Value,
) -> PreToolDecision {
    let rules = &session.config.hooks.pre_tool_use;
    if rules.is_empty() {
        return PreToolDecision::Allow;
    }
    let ctx = HookCtx {
        event: PRE_TOOL_USE,
        tool: Some(tool.to_string()),
        session_id: session.id.to_string(),
        cwd: session.meta().cwd,
        input: Some(input.clone()),
        output: None,
    };
    let (decision, warnings) = eval_pre(rules, tool, &ctx).await;
    for text in warnings {
        warn_notice(session, agent, text);
    }
    decision
}

/// `post_tool_use` hooks for one finished tool call; warnings become notices.
pub async fn post_tool_use(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    tool: &str,
    input: &serde_json::Value,
    output: &serde_json::Value,
) {
    let rules = &session.config.hooks.post_tool_use;
    if rules.is_empty() {
        return;
    }
    let ctx = HookCtx {
        event: POST_TOOL_USE,
        tool: Some(tool.to_string()),
        session_id: session.id.to_string(),
        cwd: session.meta().cwd,
        input: Some(input.clone()),
        output: Some(output.clone()),
    };
    for text in eval_post(rules, tool, &ctx).await {
        warn_notice(session, agent, text);
    }
}

/// Sessions whose `session_start` hooks already fired. The fire is lazy — on
/// the first turn, since the session-open path lives in `core/open.rs` — and
/// the set dedups the turns of one session and the main-agent rebuild a turn
/// detach performs. Entries stay for the daemon's lifetime (one id string per
/// session).
static SESSION_STARTED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Fire `session_start` hooks once per session, on its first turn.
/// Fire-and-forget: failures surface as notices, never block the turn.
pub fn session_start(session: &Arc<Session>, agent: &Arc<Agent>) {
    if session.config.hooks.session_start.is_empty() {
        return;
    }
    let already_fired = SESSION_STARTED
        .lock()
        .map(|mut fired| !fired.insert(session.id.to_string()))
        .unwrap_or(true);
    if already_fired {
        return;
    }
    let ctx = HookCtx {
        event: SESSION_START,
        tool: None,
        session_id: session.id.to_string(),
        cwd: session.meta().cwd,
        input: None,
        output: None,
    };
    fire_detached(
        session,
        Some(agent),
        &session.config.hooks.session_start,
        ctx,
    );
}

/// Fire `session_end` hooks. Wired by the session-close path
/// (`Core::close_session`); fire-and-forget, failures are logged.
pub fn session_end(session: &Arc<Session>) {
    if session.config.hooks.session_end.is_empty() {
        return;
    }
    let ctx = HookCtx {
        event: SESSION_END,
        tool: None,
        session_id: session.id.to_string(),
        cwd: session.meta().cwd,
        input: None,
        output: None,
    };
    fire_detached(session, None, &session.config.hooks.session_end, ctx);
}

/// Fire `notification` hooks for a permission request or user-question prompt.
/// `tool` is the tool the prompt is about, when there is one.
pub fn notification(session: &Arc<Session>, agent: &AgentId, tool: Option<String>) {
    if session.config.hooks.notification.is_empty() {
        return;
    }
    let ctx = HookCtx {
        event: NOTIFICATION,
        tool,
        session_id: session.id.to_string(),
        cwd: session.meta().cwd,
        input: None,
        output: None,
    };
    fire_detached(
        session,
        session.agent(agent).as_ref(),
        &session.config.hooks.notification,
        ctx,
    );
}

/// Spawn the matching rules of `rules` on a detached task; each failure becomes
/// a notice on `agent`, or a log line when there is no agent to attach it to.
fn fire_detached(
    session: &Arc<Session>,
    agent: Option<&Arc<Agent>>,
    rules: &[HookRule],
    ctx: HookCtx,
) {
    let rules: Vec<HookRule> = matching(rules, ctx.tool.as_deref())
        .into_iter()
        .cloned()
        .collect();
    if rules.is_empty() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        log::warn!("{} hooks skipped: no tokio runtime", ctx.event);
        return;
    };
    let session = session.clone();
    let agent = agent.cloned();
    handle.spawn(async move {
        for rule in &rules {
            let outcome = run_rule(rule, &ctx).await;
            if let Some(text) = describe_outcome(ctx.event, &rule.command, &outcome) {
                match &agent {
                    Some(agent) => warn_notice(&session, agent, text),
                    None => log::warn!("{text}"),
                }
            }
        }
    });
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod hooks_tests;
