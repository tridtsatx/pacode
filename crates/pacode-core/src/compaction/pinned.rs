//! Stage 3 of compaction: pinned data re-injected verbatim after the summary, and the
//! deterministic digest used when the summary model is unavailable.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use pacode_types::{AgentId, ContentBlock, Message, Plan, PlanStatus, TaskInfo, TaskStatus};

use super::shake::{collect_calls, key_arg};
use crate::session::Session;

/// One pinned block: markdown text re-injected verbatim after the summary.
pub type PinnedBlock = String;

/// Separates the model-written summary body from the pinned section inside a stored
/// summary, so the next (iterative) summary only sees the body, never stale pins.
pub const PINNED_MARKER: &str = "\n\n<!-- pacode:pinned -->\n";

/// Hook for features that must survive compaction verbatim (e.g. a file ownership
/// table). The default is empty; implement it on the type that owns the data.
pub trait CompactionPinned {
    fn pinned_blocks(&self) -> Vec<PinnedBlock> {
        Vec::new()
    }

    /// Blocks for the compaction of one agent's history (state that belongs to that
    /// agent, e.g. the files it read). Defaults to [`Self::pinned_blocks`].
    fn pinned_blocks_for(&self, agent: &AgentId) -> Vec<PinnedBlock> {
        let _ = agent;
        self.pinned_blocks()
    }
}

/// The compacted agent keeps a short file-ledger digest: files it read or wrote
/// recently, their versions, and outstanding "modified by another agent" warnings.
impl CompactionPinned for Session {
    fn pinned_blocks_for(&self, agent: &AgentId) -> Vec<PinnedBlock> {
        self.ledger
            .digest(agent, &self.meta().cwd)
            .into_iter()
            .collect()
    }
}

/// Split a stored summary into (body, pinned section).
pub fn split_pinned(summary: &str) -> (&str, Option<&str>) {
    match summary.split_once(PINNED_MARKER) {
        Some((body, pinned)) => (body, Some(pinned)),
        None => (summary, None),
    }
}

/// Append pinned blocks to a summary body.
pub fn attach_pinned(body: &str, pinned: &[PinnedBlock]) -> String {
    let blocks: Vec<&str> = pinned
        .iter()
        .map(String::as_str)
        .filter(|b| !b.trim().is_empty())
        .collect();
    if blocks.is_empty() {
        return body.trim_end().to_string();
    }
    format!("{}{PINNED_MARKER}{}", body.trim_end(), blocks.join("\n\n"))
}

pub fn plan_block(plan: &Plan) -> Option<PinnedBlock> {
    if plan.is_empty() {
        return None;
    }
    let mut out = String::from("## Current plan (verbatim)\n");
    for item in &plan.items {
        let mark = match item.status {
            PlanStatus::Pending => "[ ]",
            PlanStatus::Active => "[~]",
            PlanStatus::Done => "[x]",
            PlanStatus::Cancelled => "[-]",
        };
        let _ = write!(
            out,
            "{mark} {} ({}): {}",
            item.id,
            status_word(item.status),
            item.content
        );
        if let Some(p) = item.progress {
            let _ = write!(out, " [{p}%]");
        }
        out.push('\n');
    }
    Some(out.trim_end().to_string())
}

fn status_word(status: PlanStatus) -> &'static str {
    match status {
        PlanStatus::Pending => "pending",
        PlanStatus::Active => "active",
        PlanStatus::Done => "done",
        PlanStatus::Cancelled => "cancelled",
    }
}

pub fn tasks_block(tasks: &[TaskInfo]) -> Option<PinnedBlock> {
    let running: Vec<&TaskInfo> = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Running && t.backgrounded)
        .collect();
    if running.is_empty() {
        return None;
    }
    let mut out = String::from("## Active background tasks (verbatim)\n");
    for t in running {
        let _ = writeln!(out, "- {} `{}` (cwd {})", t.id, t.command, t.cwd.display());
    }
    Some(out.trim_end().to_string())
}

/// Pointers (not contents) to user rules: AGENTS.md / PACODE.md / CLAUDE.md files that
/// apply to `cwd`, nearest directory last.
pub fn instruction_pointer_block(cwd: &Path) -> Option<PinnedBlock> {
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    dirs.reverse();
    let mut found = Vec::new();
    for dir in dirs {
        for name in ["AGENTS.md", "PACODE.md", "CLAUDE.md"] {
            let path = dir.join(name);
            if path.is_file() {
                found.push(path);
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## User rules\nProject instructions stay in your system prompt and apply unchanged; \
         re-read these files if unsure:\n",
    );
    for path in found {
        let _ = writeln!(out, "- {}", path.display());
    }
    Some(out.trim_end().to_string())
}

/// All pinned blocks for compacting `agent`: plan, background tasks, rules pointers,
/// then the [`CompactionPinned`] hook.
pub fn collect_pinned(session: &Session, agent: &AgentId) -> Vec<PinnedBlock> {
    let mut blocks = Vec::new();
    let plan = session
        .plan
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    blocks.extend(plan_block(&plan));
    blocks.extend(tasks_block(&session.tasks_of_session()));
    blocks.extend(instruction_pointer_block(&session.meta().cwd));
    blocks.extend(session.pinned_blocks_for(agent));
    blocks
}

const DIGEST_MAX_ITEMS: usize = 40;

/// Deterministic digest of a message span (files touched, commands run) used instead of
/// an LLM summary when the summary call fails.
pub fn digest(messages: &[Arc<Message>]) -> String {
    let calls = collect_calls(messages);
    let mut written: Vec<String> = Vec::new();
    let mut read: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    for msg in messages {
        for block in &msg.content {
            let ContentBlock::ToolResult {
                call_id, is_error, ..
            } = block
            else {
                continue;
            };
            let Some((name, input)) = calls.get(call_id) else {
                continue;
            };
            let key = key_arg(input).unwrap_or_default();
            let target = match *name {
                "write" | "edit" | "multi_edit" => &mut written,
                "read" => &mut read,
                "bash" => {
                    let outcome = if *is_error { "failed" } else { "ok" };
                    commands.push(format!("`{key}` -> {outcome}"));
                    continue;
                }
                _ => continue,
            };
            let entry = format!("{name} {key}");
            if !target.contains(&entry) {
                target.push(entry);
            }
        }
    }
    let mut out = String::from(
        "[Automatic digest: the summary model was unavailable, so older history was \
         reduced to this list of actions.]\n",
    );
    push_list(&mut out, "Files modified", &written);
    push_list(&mut out, "Files read", &read);
    push_list(&mut out, "Commands run", &commands);
    out.trim_end().to_string()
}

fn push_list(out: &mut String, title: &str, items: &[String]) {
    let _ = writeln!(out, "## {title}");
    if items.is_empty() {
        out.push_str("(none)\n");
        return;
    }
    // Keep the most recent entries when over the cap.
    let skip = items.len().saturating_sub(DIGEST_MAX_ITEMS);
    if skip > 0 {
        let _ = writeln!(out, "(... {skip} earlier entries omitted)");
    }
    for item in items.iter().skip(skip) {
        let _ = writeln!(out, "- {item}");
    }
}
