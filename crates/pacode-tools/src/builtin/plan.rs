//! `plan`: the session plan shown in the rail (spec §10).

use async_trait::async_trait;
use pacode_types::{Plan, PlanItem, PlanStatus};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{parse_input, preview_body};
use crate::{DefinitionCache, Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "plan";

#[derive(Default)]
pub struct PlanTool {
    definition: DefinitionCache,
}

/// Cap on one plan item's text. The plan is re-rendered into the rail and comes
/// back on every `get`, so a 20 KB "step" is paid for on every model step from
/// then on; 500 chars is room for a real one-line step.
pub const MAX_ITEM_CHARS: usize = 500;
/// Cap on the number of items a `set` may store. A plan longer than this is a
/// document, and a document belongs in a file the agent reads, not in the rail.
pub const MAX_ITEMS: usize = 20;
/// Cap on the characters of `content` a single `set` stores in total. Sits
/// below `registry::MAX_TOOL_OUTPUT_BYTES` deliberately: a plan longer than the
/// hard byte cap would come back from `get` cut with a marker the model cannot
/// act on, since it cannot set a plan shorter than the one it already has.
pub const MAX_SET_CHARS: usize = 8_000;

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlanItemInput {
    id: Option<String>,
    content: String,
    status: Option<String>,
    progress: Option<u8>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlanInput {
    action: String,
    items: Option<Vec<PlanItemInput>>,
    item_id: Option<String>,
    status: Option<String>,
    progress: Option<u8>,
}

fn parse_plan_status(s: &str) -> Result<PlanStatus, ToolError> {
    match s {
        "pending" => Ok(PlanStatus::Pending),
        "active" => Ok(PlanStatus::Active),
        "done" => Ok(PlanStatus::Done),
        "cancelled" => Ok(PlanStatus::Cancelled),
        other => Err(ToolError::invalid(format!("invalid status: {other}"))),
    }
}

/// Apply one `update` to `plan`: set the item's status (`done` clears progress)
/// and/or progress. Activating an item demotes every other `active` item back
/// to `pending` — the description promises exactly one `active` at a time.
fn apply_update(
    plan: &mut Plan,
    item_id: &str,
    status: Option<&str>,
    progress: Option<u8>,
) -> Result<(), ToolError> {
    let idx = plan
        .items
        .iter()
        .position(|i| i.id == item_id)
        .ok_or_else(|| ToolError::invalid(format!("plan item not found: {item_id}")))?;
    let item = &mut plan.items[idx];

    if let Some(status_str) = status {
        let new_status = parse_plan_status(status_str)?;
        item.status = new_status;
        if new_status == PlanStatus::Done {
            item.progress = None;
        }
    }
    if let Some(p) = progress.filter(|_| item.status != PlanStatus::Done) {
        item.progress = Some(p);
    }

    if item.status == PlanStatus::Active {
        for (i, other) in plan.items.iter_mut().enumerate() {
            if i != idx && other.status == PlanStatus::Active {
                other.status = PlanStatus::Pending;
                other.progress = None;
            }
        }
    }
    Ok(())
}

/// Clip one item's text to [`MAX_ITEM_CHARS`] and to what is left of the payload
/// budget. Returns the text kept and how many characters were dropped from it, so
/// the caller can tell the model rather than silently storing a shortened step.
///
/// `budget` is a hard ceiling, not a target: the returned text is never longer
/// than it. An earlier version added one to the allowance so that an item landing
/// on an exactly-exhausted budget would still report a cut — but the cut is
/// reported from `dropped`, which is computed against the full length, so the
/// `+1` bought nothing and cost a character of overrun per plan.
fn bound_item(content: String, budget: usize) -> (String, usize) {
    let total = content.chars().count();
    let allowed = MAX_ITEM_CHARS.min(budget);
    if total <= allowed {
        return (content, 0);
    }
    let kept: String = content.chars().take(allowed).collect();
    let kept = kept.trim_end().to_string();
    let dropped = total.saturating_sub(kept.chars().count());
    (kept, dropped)
}

/// The line that tells the model its plan came back short. A capped `set` that
/// said nothing would look exactly like a successful one, and the model would
/// go on believing in steps that are not in the store.
fn truncation_note(dropped_items: usize, dropped_chars: usize) -> Option<String> {
    if dropped_items == 0 && dropped_chars == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if dropped_items > 0 {
        parts.push(format!(
            "{dropped_items} item(s) past the {MAX_ITEMS}-item limit"
        ));
    }
    if dropped_chars > 0 {
        parts.push(format!(
            "{dropped_chars} character(s) past the {MAX_ITEM_CHARS}-char per-item and \
             {MAX_SET_CHARS}-char per-call limits"
        ));
    }
    Some(format!(
        "[plan input truncated: {} — keep step text to one line; the rest of the detail goes \
         in a file, not in the plan]\n",
        parts.join("; ")
    ))
}

pub fn render_plan(plan: &Plan) -> String {
    if plan.items.is_empty() {
        return "No plan set.".to_string();
    }
    let mut lines = Vec::new();
    for item in &plan.items {
        let marker = match item.status {
            PlanStatus::Done => "[x]",
            PlanStatus::Active => "[*]",
            PlanStatus::Pending => "[ ]",
            PlanStatus::Cancelled => "[-]",
        };
        let progress = if item.status == PlanStatus::Active {
            item.progress
                .map(|p| format!(" ({p}%)"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        lines.push(format!("{marker}{progress} {}: {}", item.id, item.content));
    }
    lines.join("\n")
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self) -> &str {
        "Maintain the visible plan for multi-step work. `set` replaces the whole list \
         (keep ids stable when re-setting); `update` changes one item's status and/or \
         progress. Exactly one item should be `active` at a time. Report `progress` \
         (0-100) only when you can measure it."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type": "string", "enum": ["set", "update", "get"]},
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["content"],
                        "properties": {
                            "id": {"type": "string"},
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "active", "done", "cancelled"], "default": "pending"},
                            "progress": {"type": "integer", "minimum": 0, "maximum": 100}
                        }
                    }
                },
                "item_id": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "active", "done", "cancelled"]},
                "progress": {"type": "integer", "minimum": 0, "maximum": 100}
            }
        })
    }
    fn definition_cache(&self) -> Option<&DefinitionCache> {
        Some(&self.definition)
    }

    fn can_exceed_output_cap(&self) -> bool {
        false
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `set`: build `Plan{version: old+1, items}` assigning ids `p1..pN` when missing;
    /// `update`: [`apply_update`] (unknown id → InvalidInput; `done` clears progress;
    /// a new `active` demotes the previous one); `get`: render the plan.
    /// Output = rendered plan (`[x]`/`[*] 60%`/`[ ]` lines).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<PlanInput>(input)?;
        match args.action.as_str() {
            "set" => {
                let items_in = args.items.unwrap_or_default();
                let mut plan = ctx.host.plan();
                plan.version += 1;
                let dropped_items = items_in.len().saturating_sub(MAX_ITEMS);
                let mut dropped_chars = 0usize;
                let mut budget = MAX_SET_CHARS;
                let mut new_items = Vec::with_capacity(items_in.len().min(MAX_ITEMS));
                for (i, in_item) in items_in.into_iter().take(MAX_ITEMS).enumerate() {
                    let id = in_item
                        .id
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| format!("p{}", i + 1));
                    let status = match in_item.status.as_deref() {
                        Some(s) => parse_plan_status(s)?,
                        None => PlanStatus::Pending,
                    };
                    let progress = if status == PlanStatus::Done {
                        None
                    } else {
                        in_item.progress
                    };
                    let (content, cut) = bound_item(in_item.content, budget);
                    budget = budget.saturating_sub(content.chars().count());
                    dropped_chars += cut;
                    new_items.push(PlanItem {
                        id,
                        content,
                        status,
                        progress,
                    });
                }
                plan.items = new_items;
                ctx.host.set_plan(plan.clone());
                let rendered = render_plan(&plan);
                let content_out = match truncation_note(dropped_items, dropped_chars) {
                    Some(note) => format!("{note}\n{rendered}"),
                    None => rendered,
                };
                let preview = preview_body(&content_out);
                Ok(ToolOutput::text(content_out)
                    .with_title("Plan")
                    .with_preview(preview))
            }
            "update" => {
                let item_id = args
                    .item_id
                    .ok_or_else(|| ToolError::invalid("item_id is required for update"))?;
                let mut plan = ctx.host.plan();
                apply_update(&mut plan, &item_id, args.status.as_deref(), args.progress)?;

                plan.version += 1;
                ctx.host.set_plan(plan.clone());
                let rendered = render_plan(&plan);
                let preview = preview_body(&rendered);
                Ok(ToolOutput::text(rendered)
                    .with_title("Plan update")
                    .with_preview(preview))
            }
            "get" => {
                let plan = ctx.host.plan();
                let rendered = render_plan(&plan);
                let preview = preview_body(&rendered);
                Ok(ToolOutput::text(rendered)
                    .with_title("Plan")
                    .with_preview(preview))
            }
            other => Err(ToolError::invalid(format!("unknown action: {other}"))),
        }
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod plan_tests;
