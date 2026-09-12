//! `batch`: several independent read-only ops in one call (spec §7).
//!
//! Ops resolve through `ctx.tools` — the agent's live registry — so a tool
//! disabled via `tools.<name>.enabled = false` fails inside a batch exactly as
//! it would standalone. `bash` ops must additionally satisfy
//! `pacode_risk::is_read_only`; instead of bash's own per-call permission the
//! batch asks once for the whole set through a single `PermissionDraft` that
//! lists every op, carrying the max risk across the commands.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::bash::{BashInput, run_authorized};
use super::helpers::{cap_output, parse_input, preview_body};
use crate::host::PermissionDraft;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};
use pacode_types::PermissionDecision;

pub const NAME: &str = "batch";

/// Hard cap on ops per call.
const MAX_OPS: usize = 8;

/// Read-only tools an op may invoke. `bash` is further restricted to commands
/// passing `pacode_risk::is_read_only` (the same check Plan mode applies).
const ALLOWED_OPS: &[&str] = &["read", "grep", "glob", "ls", "bash"];

pub struct BatchTool;

#[derive(Deserialize, Default)]
#[serde(default)]
struct BatchInput {
    ops: Vec<Value>,
}

/// One op after normalization and validation.
enum PreparedOp {
    Ready {
        /// Short signature for result headers and the prompt's first line.
        label: String,
        name: String,
        input: Value,
        /// Set for bash ops only.
        risk: Option<pacode_risk::RiskAssessment>,
    },
    Rejected {
        label: String,
        reason: String,
    },
}

struct OpResult {
    label: String,
    ok: bool,
    text: String,
}

#[async_trait]
impl Tool for BatchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Run several independent read-only lookups in one call — file reads, content \
         search, file globs, directory listings and read-only shell commands run in \
         parallel and come back in the order given. Prefer this over issuing the same \
         calls one by one when exploring; each op's output is capped to a fair share \
         of the output budget, so for one big result call the tool directly instead. \
         Writes are refused: editing tools and mutating shell commands never run in a \
         batch. Shell ops share a single permission prompt covering the whole batch; \
         refusing it fails the call."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["ops"],
            "properties": {
                "ops": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_OPS,
                    "items": {
                        "type": "object",
                        "required": ["tool", "input"],
                        "properties": {
                            "tool": {"type": "string", "enum": ALLOWED_OPS},
                            "input": {"type": "object", "description": "Arguments for that tool, same shape as a direct call."}
                        }
                    }
                }
            }
        })
    }

    /// ReadOnly so the batch joins the concurrent chunk; the Exec permission
    /// behaviour of shell ops comes from the explicit draft kind, not this.
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large) = parse_input::<BatchInput>(input)?;
        if args.ops.is_empty() {
            return Err(ToolError::invalid("`ops` is required (1..=8 operations)"));
        }
        if args.ops.len() > MAX_OPS {
            return Err(ToolError::invalid(format!(
                "batch accepts at most {MAX_OPS} operations, got {}",
                args.ops.len()
            )));
        }

        let ops: Vec<PreparedOp> = args.ops.iter().map(|raw| prepare(raw, ctx)).collect();

        // One confirmation for the whole batch — the first detail line doubles
        // as the AllowSession cache key, so it must cover every op, not just
        // the count.
        if ops
            .iter()
            .any(|op| matches!(op, PreparedOp::Ready { name, .. } if name.as_str() == "bash"))
        {
            ask_batch_permission(&ops, ctx).await?;
        }

        // Fair share of the output budget per op; the op's own
        // `accept_large_output` still applies inside its share.
        let per_op_cap = if accept_large {
            ctx.output_cap_chars
        } else {
            (ctx.output_cap_chars / ops.len()).max(1)
        };

        let futures = ops.into_iter().map(|op| {
            let ctx = ctx.clone();
            async move { run_op(op, &ctx, per_op_cap).await }
        });
        let results = futures::future::join_all(futures).await;

        let total = results.len();
        let succeeded = results.iter().filter(|r| r.ok).count();
        let failed = total - succeeded;

        let mut body = String::new();
        for (index, result) in results.iter().enumerate() {
            body.push_str(&format!(
                "--- [{}] {} ---\n{}\n",
                index + 1,
                result.label,
                result.text
            ));
        }
        body.push_str(&format!("\nBatch: {succeeded} succeeded, {failed} failed"));

        let content = cap_output(&body, accept_large, ctx.output_cap_chars);
        let mut out = ToolOutput::text(content)
            .with_title(format!("Batch {total} ops"))
            .with_preview(format!(
                "{succeeded}/{total} ops ok\n{}",
                preview_body(&body)
            ));
        // Only a fully failed batch is an error; partial failures keep their
        // successful sections readable.
        out.is_error = failed == total;
        Ok(out)
    }
}

/// Accept `{tool, input}` plus the aliases models actually emit — `name` for
/// the tool, `parameters`/`args`/`arguments` for the input, and flat
/// `{tool, path}` shapes where non-structural keys fold into `input`.
fn normalize_op(raw: &Value) -> Result<(String, Value), String> {
    let Some(map) = raw.as_object() else {
        return Err("op must be an object".to_string());
    };
    let name = ["tool", "name"]
        .iter()
        .filter_map(|key| map.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string);
    let mut input = ["input", "parameters", "args", "arguments"]
        .iter()
        .find_map(|key| map.get(*key).and_then(Value::as_object).cloned())
        .map(Value::Object)
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    const STRUCTURAL: &[&str] = &["tool", "name", "input", "parameters", "args", "arguments"];
    for (key, value) in map {
        if !STRUCTURAL.contains(&key.as_str()) {
            input[key.as_str()] = value.clone();
        }
    }
    match name {
        Some(name) => Ok((name, input)),
        None => Err("op has no `tool` name".to_string()),
    }
}

fn prepare(raw: &Value, ctx: &ToolCtx) -> PreparedOp {
    let (name, input) = match normalize_op(raw) {
        Ok(pair) => pair,
        Err(reason) => {
            return PreparedOp::Rejected {
                label: "op".to_string(),
                reason,
            };
        }
    };
    let label = describe(&name, &input);

    if !ALLOWED_OPS.contains(&name.as_str()) {
        return PreparedOp::Rejected {
            label,
            reason: format!(
                "`{name}` is not allowed in batch (allowed: {})",
                ALLOWED_OPS.join(", ")
            ),
        };
    }
    // A disabled tool is absent from the agent's registry — the batch must not
    // resurrect it.
    if ctx.tools.get(&name).is_none() {
        return PreparedOp::Rejected {
            label,
            reason: format!("`{name}` is disabled or not registered in this session"),
        };
    }
    if name == "bash" {
        let args: BashInput = match serde_json::from_value(input.clone()) {
            Ok(args) => args,
            Err(e) => {
                return PreparedOp::Rejected {
                    label,
                    reason: format!("invalid bash input: {e}"),
                };
            }
        };
        if args.command.trim().is_empty() {
            return PreparedOp::Rejected {
                label,
                reason: "empty command".to_string(),
            };
        }
        // A batch is a synchronous fan-out; detaching a task would leave work
        // the caller never sees and bypass the result ordering.
        if args.background {
            return PreparedOp::Rejected {
                label,
                reason: "background tasks are not allowed in batch".to_string(),
            };
        }
        if !pacode_risk::is_read_only(&args.command) {
            return PreparedOp::Rejected {
                label,
                reason: format!("not a read-only command: {}", args.command),
            };
        }
        return PreparedOp::Ready {
            label,
            name,
            input,
            risk: Some(pacode_risk::classify(&args.command, &ctx.cwd)),
        };
    }
    PreparedOp::Ready {
        label,
        name,
        input,
        risk: None,
    }
}

/// The single permission request covering every runnable op. `Exec` kind makes
/// the mode/risk matrix see this as a shell call; the max risk across commands
/// decides Ask/Allow/Deny.
async fn ask_batch_permission(ops: &[PreparedOp], ctx: &ToolCtx) -> Result<(), ToolError> {
    let runnable: Vec<&PreparedOp> = ops
        .iter()
        .filter(|op| matches!(op, PreparedOp::Ready { .. }))
        .collect();
    let max_risk = ops
        .iter()
        .filter_map(|op| match op {
            PreparedOp::Ready { risk, .. } => risk.as_ref().map(|a| a.level),
            PreparedOp::Rejected { .. } => None,
        })
        .max();

    let summary = runnable
        .iter()
        .map(|op| match op {
            PreparedOp::Ready { label, .. } => label.clone(),
            PreparedOp::Rejected { label, .. } => label.clone(),
        })
        .collect::<Vec<_>>()
        .join("; ");

    let mut detail = format!("{summary}\n\n{} operation(s):", runnable.len());
    for (index, op) in runnable.iter().enumerate() {
        let line = match op {
            PreparedOp::Ready { name, input, .. } => review_line(name, input),
            PreparedOp::Rejected { label, .. } => label.clone(),
        };
        detail.push_str(&format!("\n{}. {line}", index + 1));
    }
    let risk_notes: Vec<String> = ops
        .iter()
        .filter_map(|op| match op {
            PreparedOp::Ready {
                risk: Some(assessment),
                input,
                ..
            } => assessment.summary().map(|summary| {
                let command = input
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                format!("{command}: {summary}")
            }),
            PreparedOp::Ready { risk: None, .. } | PreparedOp::Rejected { .. } => None,
        })
        .collect();
    if !risk_notes.is_empty() {
        detail.push_str(&format!("\n\nRisk: {}", risk_notes.join("; ")));
    }

    let draft = PermissionDraft::new(
        ctx.agent.clone(),
        ctx.agent_name.clone(),
        ctx.call_id.clone(),
        format!("Batch of {} operation(s)", runnable.len()),
        detail,
        max_risk,
    )
    .with_tool(NAME, ToolKind::Exec);
    match ctx.host.request_permission(draft).await {
        PermissionDecision::AllowOnce | PermissionDecision::AllowSession => Ok(()),
        PermissionDecision::Deny => Err(ToolError::Denied("user denied the batch".to_string())),
    }
}

async fn run_op(op: PreparedOp, ctx: &ToolCtx, per_op_cap: usize) -> OpResult {
    match op {
        PreparedOp::Rejected { label, reason } => OpResult {
            label,
            ok: false,
            text: format!("Error: {reason}"),
        },
        PreparedOp::Ready {
            label, name, input, ..
        } => {
            let tool = ctx.tools.get(&name);
            let kind = tool
                .as_ref()
                .map(|t| t.kind())
                .unwrap_or(ToolKind::ReadOnly);
            let mut sub_ctx = ctx.clone().with_tool(name.clone(), kind);
            sub_ctx.output_cap_chars = per_op_cap;
            // Bash ops take `run_authorized` directly: classification and the
            // (combined) permission were settled in `prepare`/`ask_batch_permission`.
            let result = if name == "bash" {
                match parse_input::<BashInput>(input) {
                    Ok((args, accept)) => run_authorized(args, accept, &sub_ctx).await,
                    Err(err) => Err(err),
                }
            } else {
                match tool {
                    Some(tool) => tool.call(input, &sub_ctx).await,
                    None => Err(ToolError::failed(format!(
                        "tool `{name}` became unavailable"
                    ))),
                }
            };
            match result {
                Ok(out) => OpResult {
                    label,
                    ok: !out.is_error,
                    text: out.content,
                },
                Err(err) => OpResult {
                    label,
                    ok: false,
                    text: format!("Error: {err}"),
                },
            }
        }
    }
}

/// Short op signature for result headers (truncated at 80 chars).
fn describe(name: &str, input: &Value) -> String {
    let raw = review_line(name, input);
    raw.chars().take(80).collect()
}

/// Full op signature for the permission detail — commands are shown whole so
/// the review sees exactly what runs.
fn review_line(name: &str, input: &Value) -> String {
    let get = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    match name {
        "read" => format!("read {}", get("path")),
        "grep" => format!("grep \"{}\"", get("pattern")),
        "glob" => format!("glob \"{}\"", get("pattern")),
        "ls" => {
            let path = get("path");
            format!("ls {}", if path.is_empty() { "." } else { path })
        }
        "bash" => format!("bash: {}", get("command")),
        other => format!("{other} {}", get("path")).trim_end().to_string(),
    }
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod batch_tests;
