//! `plan`: the session plan shown in the rail (spec §10).

use async_trait::async_trait;
use pacode_types::{Plan, PlanItem, PlanStatus};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::parse_input;
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "plan";

pub struct PlanTool;

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

    fn kind(&self) -> ToolKind {
        ToolKind::Control
    }

    /// `set`: build `Plan{version: old+1, items}` assigning ids `p1..pN` when missing;
    /// `update`: modify the item (unknown id → InvalidInput); `done` clears progress;
    /// `get`: render the plan. Output = rendered plan (`[x]`/`[*] 60%`/`[ ]` lines).
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _accept_large_output) = parse_input::<PlanInput>(input)?;
        match args.action.as_str() {
            "set" => {
                let items_in = args.items.unwrap_or_default();
                let mut plan = ctx.host.plan();
                plan.version += 1;
                let mut new_items = Vec::with_capacity(items_in.len());
                for (i, in_item) in items_in.into_iter().enumerate() {
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
                    new_items.push(PlanItem {
                        id,
                        content: in_item.content,
                        status,
                        progress,
                    });
                }
                plan.items = new_items;
                ctx.host.set_plan(plan.clone());
                let rendered = render_plan(&plan);
                let preview = format!("{} items", plan.items.len());
                Ok(ToolOutput::text(rendered)
                    .with_title("Plan")
                    .with_preview(preview))
            }
            "update" => {
                let item_id = args
                    .item_id
                    .ok_or_else(|| ToolError::invalid("item_id is required for update"))?;
                let mut plan = ctx.host.plan();
                let item = plan
                    .items
                    .iter_mut()
                    .find(|i| i.id == item_id)
                    .ok_or_else(|| ToolError::invalid(format!("plan item not found: {item_id}")))?;

                if let Some(status_str) = args.status.as_deref() {
                    let new_status = parse_plan_status(status_str)?;
                    item.status = new_status;
                    if new_status == PlanStatus::Done {
                        item.progress = None;
                    }
                }
                if let Some(p) = args.progress.filter(|_| item.status != PlanStatus::Done) {
                    item.progress = Some(p);
                }

                plan.version += 1;
                ctx.host.set_plan(plan.clone());
                let rendered = render_plan(&plan);
                let preview = format!("updated {item_id}");
                Ok(ToolOutput::text(rendered)
                    .with_title("Plan update")
                    .with_preview(preview))
            }
            "get" => {
                let plan = ctx.host.plan();
                let rendered = render_plan(&plan);
                let preview = format!("{} items", plan.items.len());
                Ok(ToolOutput::text(rendered)
                    .with_title("Plan")
                    .with_preview(preview))
            }
            other => Err(ToolError::invalid(format!("unknown action: {other}"))),
        }
    }
}
