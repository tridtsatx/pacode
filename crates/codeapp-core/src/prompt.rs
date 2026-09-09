//! System prompt (spec §6.4). Static part first (prefix cache), dynamic part second.

use std::path::Path;

use codeapp_types::{Mode, Plan};

/// Inputs of the dynamic part.
pub struct DynamicContext<'a> {
    pub cwd: &'a Path,
    pub git_branch: Option<&'a str>,
    pub date: &'a str,
    pub mode: Mode,
    pub plan: &'a Plan,
    /// AGENTS.md / CLAUDE.md content, already capped.
    pub instructions: Option<&'a str>,
    /// `true` for subagents (no `agent` tool, report back at the end).
    pub is_subagent: bool,
}

/// Identity, tool rules, the background-first thesis, plan rules, permission notes.
/// Must be byte-stable across turns of a session.
pub fn system_static() -> &'static str {
    "You are codeapp, an autonomous CLI coding agent built around background work.\n\n\
    ## Tool Semantics\n\
    You have access to tools to inspect, modify, and manage the workspace. Use tools to execute tasks directly rather than giving instructions whenever possible.\n\n\
    ## Background-First Thesis\n\
    When starting long-running builds, tests, or servers, run them in the background. Continue executing other steps while background tasks run. You will receive task completions and output via injected events between steps.\n\n\
    ## Subagents\n\
    Use the `agent` tool to run independent work in parallel. `spawn` returns immediately; the subagent's final report and its `report_status` updates arrive as `<agent_finished>` / `<agent_status>` events between your steps. Use `agent ask_status` to request a brief status from a running subagent; use `wait` only when you cannot proceed without the result.\n\n\
    ## Planning\n\
    For any task with 3 or more steps, use the `plan` tool to create and maintain a clear plan. Keep items up to date. Report `progress` only when it is measurable.\n\n\
    ## Permissions\n\
    Actions are gated based on the session's security mode (Build, Auto, Plan, Bypass). High-risk or catastrophic actions may be denied or require user confirmation. Respect tool denials and adapt.\n\n\
    ## Output Rules\n\
    - Answer in the same language as the user's prompt.\n\
    - Be concise and direct. Avoid unnecessary conversational filler, preambles, or summaries."
}

pub fn system_dynamic(ctx: &DynamicContext<'_>) -> String {
    let mut out = String::new();
    out.push_str("## Environment\n");
    out.push_str(&format!("- Working directory: {}\n", ctx.cwd.display()));
    if let Some(branch) = ctx.git_branch {
        out.push_str(&format!("- Git branch: {branch}\n"));
    }
    out.push_str(&format!("- Current date: {}\n", ctx.date));
    out.push_str(&format!("- Mode: {}\n", ctx.mode.as_str()));

    if ctx.is_subagent {
        out.push_str("\n## Subagent Role\n");
        out.push_str("You are a subagent working on a dedicated subtask. The `agent` tool is not available to you. Focus on completing your assigned prompt and return your findings or results. When you receive a `<status_request>`, call `report_status` with a brief status first, then continue. Your final message is delivered to the orchestrator as your report.\n");
    }

    if !ctx.plan.is_empty() {
        out.push_str("\n## Plan Summary\n");
        out.push_str(&format!("Overall progress: {}%\n", ctx.plan.percent()));
        for item in &ctx.plan.items {
            let status_mark = match item.status {
                codeapp_types::PlanStatus::Done => "[x]",
                codeapp_types::PlanStatus::Active => "[*]",
                codeapp_types::PlanStatus::Pending => "[ ]",
                codeapp_types::PlanStatus::Cancelled => "[-]",
            };
            if let Some(p) = item.progress {
                out.push_str(&format!("- {status_mark} {} ({p}%)\n", item.content));
            } else {
                out.push_str(&format!("- {status_mark} {}\n", item.content));
            }
        }
    }

    if let Some(instructions) = ctx.instructions
        && !instructions.trim().is_empty()
    {
        out.push_str("\n## Project Instructions\n");
        out.push_str(instructions);
        out.push('\n');
    }

    out
}

/// Collect `AGENTS.md` and `CLAUDE.md` from `cwd` up to the filesystem root (nearest
/// last so it wins), plus `~/.config/codeapp/AGENTS.md` first. Each file is prefixed
/// with `# <path>`; the total is capped to `cap_chars` (head+tail).
pub fn load_instructions(
    cwd: &Path,
    home_config: Option<&Path>,
    cap_chars: usize,
) -> Option<String> {
    let mut files = Vec::new();

    // 1. home_config/AGENTS.md first
    if let Some(home) = home_config {
        let home_agents = home.join("AGENTS.md");
        if home_agents.is_file() {
            files.push(home_agents);
        }
    }

    // 2. From filesystem root down to cwd (nearest last so it wins)
    let mut ancestors: Vec<&Path> = cwd.ancestors().collect();
    ancestors.reverse();

    for dir in ancestors {
        let agents = dir.join("AGENTS.md");
        if agents.is_file() && !files.contains(&agents) {
            files.push(agents);
        }
        let claude = dir.join("CLAUDE.md");
        if claude.is_file() && !files.contains(&claude) {
            files.push(claude);
        }
    }

    if files.is_empty() {
        return None;
    }

    let mut combined = String::new();
    for file in files {
        if let Ok(content) = std::fs::read_to_string(&file)
            && !content.trim().is_empty()
        {
            if !combined.is_empty() {
                combined.push_str("\n\n");
            }
            combined.push_str(&format!("# {}\n\n{}", file.display(), content.trim()));
        }
    }

    if combined.is_empty() {
        return None;
    }

    Some(codeapp_types::truncate_head_tail(&combined, cap_chars))
}

/// `git rev-parse --abbrev-ref HEAD` equivalent by reading `.git/HEAD` (no process).
pub fn git_branch(cwd: &Path) -> Option<String> {
    for dir in cwd.ancestors() {
        let git_entry = dir.join(".git");
        let head_file = if git_entry.is_dir() {
            git_entry.join("HEAD")
        } else if git_entry.is_file() {
            if let Ok(content) = std::fs::read_to_string(&git_entry) {
                let gitdir_path = content
                    .trim()
                    .strip_prefix("gitdir:")
                    .map(str::trim)
                    .map(|p| dir.join(p).join("HEAD"));
                match gitdir_path {
                    Some(p) if p.is_file() => p,
                    _ => continue,
                }
            } else {
                continue;
            }
        } else {
            continue;
        };

        if let Ok(head_content) = std::fs::read_to_string(&head_file) {
            let line = head_content.trim();
            if let Some(branch) = line
                .strip_prefix("ref: refs/heads/")
                .or_else(|| line.strip_prefix("ref: "))
            {
                return Some(branch.to_string());
            } else if !line.is_empty() {
                let hash_prefix = &line[..line.len().min(8)];
                return Some(hash_prefix.to_string());
            }
        }
    }
    None
}
