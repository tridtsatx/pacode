//! System prompt (spec §6.4). Static part first (prefix cache), dynamic part second.

use std::path::Path;

use pacode_types::{Mode, Plan};

/// Inputs of the dynamic part.
pub struct DynamicContext<'a> {
    pub cwd: &'a Path,
    pub git_branch: Option<&'a str>,
    pub date: &'a str,
    pub mode: Mode,
    pub plan: &'a Plan,
    /// AGENTS.md / PACODE.md / CLAUDE.md content, already capped.
    pub instructions: Option<&'a str>,
    /// `true` for subagents (no `agent` tool, report back at the end).
    pub is_subagent: bool,
    pub skills: &'a [pacode_skills::Skill],
    pub skills_enabled: bool,
    pub max_listed_skills: usize,
}

/// Identity, tool rules, the background-first thesis, plan rules, permission notes.
/// Must be byte-stable across turns of a session.
pub fn system_static() -> &'static str {
    "You are pacode, an autonomous CLI coding agent built around background work.\n\n\
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
                pacode_types::PlanStatus::Done => "[x]",
                pacode_types::PlanStatus::Active => "[*]",
                pacode_types::PlanStatus::Pending => "[ ]",
                pacode_types::PlanStatus::Cancelled => "[-]",
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

    if ctx.skills_enabled && !ctx.skills.is_empty() {
        out.push_str("\n## Skills\n");
        for skill in ctx.skills.iter().take(ctx.max_listed_skills) {
            let desc = skill.description.replace("\r\n", " ").replace('\n', " ");
            let desc = desc.trim();
            let desc_capped: String = if desc.chars().count() > 200 {
                desc.chars().take(200).collect()
            } else {
                desc.to_string()
            };
            out.push_str(&format!("- {}: {desc_capped}\n", skill.name));
        }
    }

    out
}

pub const DEFAULT_MEMORY_CAP_CHARS: usize = 8000;

/// Load global (`~/.config/pacode/memory.md`) and project (`<cwd>/.pacode/memory.md`)
/// memory files, formatted under `# Memory (global)` and `# Memory (project)`.
/// Missing or empty files are skipped. Total text is capped at `memory_cap_chars`
/// by truncating oldest entries first (i.e. keeping the tail), with a visible marker
/// consistent with [`pacode_types::truncate_head_tail`].
pub fn load_memory(
    cwd: &Path,
    home_config: Option<&Path>,
    memory_cap_chars: usize,
) -> Option<String> {
    if memory_cap_chars == 0 {
        return None;
    }

    let global_file = home_config
        .map(|h| h.join("memory.md"))
        .unwrap_or_else(|| pacode_config::Paths::discover().memory_file());

    let global_content = std::fs::read_to_string(global_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let project_file = cwd.join(".pacode").join("memory.md");
    let project_content = std::fs::read_to_string(project_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if global_content.is_none() && project_content.is_none() {
        return None;
    }

    let g_lines: Vec<&str> = global_content
        .as_deref()
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();

    let p_lines: Vec<&str> = project_content
        .as_deref()
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();

    if g_lines.is_empty() && p_lines.is_empty() {
        return None;
    }

    let render = |g: &[&str], p: &[&str]| -> String {
        let mut parts = Vec::new();
        if !g.is_empty() {
            parts.push(format!("# Memory (global)\n\n{}", g.join("\n")));
        }
        if !p.is_empty() {
            parts.push(format!("# Memory (project)\n\n{}", p.join("\n")));
        }
        parts.join("\n\n")
    };

    let full = render(&g_lines, &p_lines);
    let total_chars = full.chars().count();
    if total_chars <= memory_cap_chars {
        return Some(full);
    }

    // When cap is too small to fit the visible marker (consistent with pacode_types::truncate_head_tail),
    // truncate directly on character boundary keeping the tail.
    if memory_cap_chars < 64 {
        let tail: String = full
            .chars()
            .skip(total_chars.saturating_sub(memory_cap_chars))
            .collect();
        return if tail.is_empty() { None } else { Some(tail) };
    }

    // Visible marker consistent with truncate_head_tail format: [... N characters truncated ...]
    let marker_estimate = format!("[... {total_chars} characters truncated ...]\n\n");
    let marker_chars = marker_estimate.chars().count();
    let keep_tail_chars = memory_cap_chars.saturating_sub(marker_chars);
    let dropped = total_chars.saturating_sub(keep_tail_chars);
    let marker = format!("[... {dropped} characters truncated ...]\n\n");
    let tail: String = full.chars().skip(dropped).collect();
    Some(format!("{marker}{tail}"))
}

/// Collect `AGENTS.md`, `PACODE.md`, and `CLAUDE.md` from `cwd` up to the filesystem root
/// (order per dir: `AGENTS.md`, `PACODE.md`, `CLAUDE.md`; nearest dir last so it wins),
/// plus `~/.config/pacode/AGENTS.md` and `~/.config/pacode/PACODE.md` first. Each file is prefixed
/// with `# <path>`; the total is capped to `cap_chars` (head+tail).
/// Memory sections are loaded and appended after instructions using default memory cap.
pub fn load_instructions(
    cwd: &Path,
    home_config: Option<&Path>,
    cap_chars: usize,
) -> Option<String> {
    load_instructions_with_memory(cwd, home_config, cap_chars, DEFAULT_MEMORY_CAP_CHARS)
}

/// Variant of `load_instructions` with explicit memory cap chars.
pub fn load_instructions_with_memory(
    cwd: &Path,
    home_config: Option<&Path>,
    instructions_cap_chars: usize,
    memory_cap_chars: usize,
) -> Option<String> {
    let mut files = Vec::new();

    // 1. home_config/AGENTS.md, then home_config/PACODE.md
    if let Some(home) = home_config {
        let home_agents = home.join("AGENTS.md");
        if home_agents.is_file() {
            files.push(home_agents);
        }
        let home_pacode = home.join("PACODE.md");
        if home_pacode.is_file() {
            files.push(home_pacode);
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
        let pacode = dir.join("PACODE.md");
        if pacode.is_file() && !files.contains(&pacode) {
            files.push(pacode);
        }
        let claude = dir.join("CLAUDE.md");
        if claude.is_file() && !files.contains(&claude) {
            files.push(claude);
        }
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

    let instructions = if combined.is_empty() {
        None
    } else {
        Some(pacode_types::truncate_head_tail(
            &combined,
            instructions_cap_chars,
        ))
    };

    let memory = load_memory(cwd, home_config, memory_cap_chars);

    match (instructions, memory) {
        (Some(i), Some(m)) => Some(format!("{i}\n\n{m}")),
        (Some(i), None) => Some(i),
        (None, Some(m)) => Some(m),
        (None, None) => None,
    }
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

/// Turn-scoped prompt cache for expensive or filesystem-backed prompt components.
/// Initialized at turn start (the explicit invalidation point) and reused across
/// all steps of a single turn to preserve provider prefix caching and avoid syscalls.
#[derive(Clone, Debug)]
pub struct TurnPromptCache {
    pub git_branch: Option<String>,
    pub instructions: Option<String>,
    pub date: String,
}

impl TurnPromptCache {
    /// Invalidation point: compute fresh values at turn start.
    pub fn new(
        cwd: &Path,
        home_config: Option<&Path>,
        instructions_cap_chars: usize,
        memory_cap_chars: usize,
    ) -> Self {
        let git_branch = git_branch(cwd);
        let instructions = load_instructions_with_memory(
            cwd,
            home_config,
            instructions_cap_chars,
            memory_cap_chars,
        );
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        Self {
            git_branch,
            instructions,
            date,
        }
    }
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod prompt_tests;
