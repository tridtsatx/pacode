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
/// by truncating oldest lines first (i.e. keeping the tail).
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

    let mut g_start = 0;
    let mut p_start = 0;

    let mut rendered = render(&g_lines[g_start..], &p_lines[p_start..]);
    while rendered.chars().count() > memory_cap_chars {
        if g_start < g_lines.len() {
            g_start += 1;
        } else if p_start < p_lines.len() {
            p_start += 1;
        } else {
            break;
        }
        rendered = render(&g_lines[g_start..], &p_lines[p_start..]);
    }

    if rendered.chars().count() > memory_cap_chars {
        let skip = rendered.chars().count().saturating_sub(memory_cap_chars);
        rendered = rendered.chars().skip(skip).collect();
    }

    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_instructions_order() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home_config");
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&child).unwrap();

        std::fs::write(home.join("AGENTS.md"), "global agents").unwrap();
        std::fs::write(home.join("PACODE.md"), "global pacode").unwrap();

        std::fs::write(parent.join("AGENTS.md"), "parent agents").unwrap();
        std::fs::write(parent.join("PACODE.md"), "parent pacode").unwrap();
        std::fs::write(parent.join("CLAUDE.md"), "parent claude").unwrap();

        std::fs::write(child.join("AGENTS.md"), "child agents").unwrap();
        std::fs::write(child.join("PACODE.md"), "child pacode").unwrap();
        std::fs::write(child.join("CLAUDE.md"), "child claude").unwrap();

        let loaded = load_instructions(&child, Some(&home), 100_000).unwrap();

        let pos_global_agents = loaded.find("global agents").unwrap();
        let pos_global_pacode = loaded.find("global pacode").unwrap();
        let pos_parent_agents = loaded.find("parent agents").unwrap();
        let pos_parent_pacode = loaded.find("parent pacode").unwrap();
        let pos_parent_claude = loaded.find("parent claude").unwrap();
        let pos_child_agents = loaded.find("child agents").unwrap();
        let pos_child_pacode = loaded.find("child pacode").unwrap();
        let pos_child_claude = loaded.find("child claude").unwrap();

        assert!(pos_global_agents < pos_global_pacode);
        assert!(pos_global_pacode < pos_parent_agents);
        assert!(pos_parent_agents < pos_parent_pacode);
        assert!(pos_parent_pacode < pos_parent_claude);
        assert!(pos_parent_claude < pos_child_agents);
        assert!(pos_child_agents < pos_child_pacode);
        assert!(pos_child_pacode < pos_child_claude);
    }

    #[test]
    fn test_load_memory_skipping_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home_config");
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();

        // 1. Neither exists -> None
        assert!(load_memory(&proj, Some(&home), 8000).is_none());

        // 2. Only global exists
        std::fs::write(home.join("memory.md"), "- global note 1\n- global note 2\n").unwrap();
        let loaded_global = load_memory(&proj, Some(&home), 8000).unwrap();
        assert!(loaded_global.starts_with("# Memory (global)"));
        assert!(loaded_global.contains("- global note 1"));
        assert!(loaded_global.contains("- global note 2"));
        assert!(!loaded_global.contains("# Memory (project)"));

        // 3. Only project exists
        std::fs::remove_file(home.join("memory.md")).unwrap();
        let proj_pacode = proj.join(".pacode");
        std::fs::create_dir_all(&proj_pacode).unwrap();
        std::fs::write(proj_pacode.join("memory.md"), "- proj note 1\n").unwrap();
        let loaded_proj = load_memory(&proj, Some(&home), 8000).unwrap();
        assert!(!loaded_proj.contains("# Memory (global)"));
        assert!(loaded_proj.starts_with("# Memory (project)"));
        assert!(loaded_proj.contains("- proj note 1"));
    }

    #[test]
    fn test_load_memory_capping_keeps_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home_config");
        let proj = tmp.path().join("proj");
        let proj_pacode = proj.join(".pacode");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj_pacode).unwrap();

        // Global has older notes; project has newer notes
        std::fs::write(
            home.join("memory.md"),
            "- global line 1 (oldest)\n- global line 2\n- global line 3\n",
        )
        .unwrap();
        std::fs::write(
            proj_pacode.join("memory.md"),
            "- proj line 1\n- proj line 2 (newest)\n",
        )
        .unwrap();

        let full = load_memory(&proj, Some(&home), 8000).unwrap();
        assert!(full.contains("global line 1"));
        assert!(full.contains("proj line 2"));

        // Cap to length that drops oldest global line(s)
        let capped = load_memory(&proj, Some(&home), 90).unwrap();
        assert!(capped.chars().count() <= 90);
        // Newest line must be kept
        assert!(capped.contains("proj line 2 (newest)"));
        // Oldest global line must be dropped
        assert!(!capped.contains("global line 1 (oldest)"));

        // Even tighter cap drops all global lines and keeps only project tail
        let tight = load_memory(&proj, Some(&home), 50).unwrap();
        assert!(tight.chars().count() <= 50);
        assert!(!tight.contains("# Memory (global)"));
        assert!(tight.contains("proj line 2 (newest)"));
    }

    #[test]
    fn test_load_instructions_appends_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home_config");
        let proj = tmp.path().join("proj");
        let proj_pacode = proj.join(".pacode");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj_pacode).unwrap();

        std::fs::write(proj.join("AGENTS.md"), "instruction content").unwrap();
        std::fs::write(home.join("memory.md"), "- remember global").unwrap();
        std::fs::write(proj_pacode.join("memory.md"), "- remember project").unwrap();

        let loaded = load_instructions(&proj, Some(&home), 10_000).unwrap();
        assert!(loaded.contains("instruction content"));
        assert!(loaded.contains("# Memory (global)"));
        assert!(loaded.contains("- remember global"));
        assert!(loaded.contains("# Memory (project)"));
        assert!(loaded.contains("- remember project"));

        let pos_instr = loaded.find("instruction content").unwrap();
        let pos_global = loaded.find("# Memory (global)").unwrap();
        let pos_proj = loaded.find("# Memory (project)").unwrap();

        assert!(pos_instr < pos_global);
        assert!(pos_global < pos_proj);
    }

    #[test]
    fn test_system_dynamic_skills_section() {
        let cwd = std::path::PathBuf::from("/test/cwd");
        let plan = pacode_types::Plan::default();

        let long_desc = "x".repeat(300);
        let skills = vec![
            pacode_skills::Skill {
                name: "skill-a".to_string(),
                description: "Description for skill A".to_string(),
                dir: cwd.join("skills/skill-a"),
                path: cwd.join("skills/skill-a/SKILL.md"),
            },
            pacode_skills::Skill {
                name: "skill-b".to_string(),
                description: long_desc,
                dir: cwd.join("skills/skill-b"),
                path: cwd.join("skills/skill-b/SKILL.md"),
            },
            pacode_skills::Skill {
                name: "skill-c".to_string(),
                description: "Skill C should be omitted if max_listed is 2".to_string(),
                dir: cwd.join("skills/skill-c"),
                path: cwd.join("skills/skill-c/SKILL.md"),
            },
        ];

        // 1. Skills enabled and present
        let ctx = DynamicContext {
            cwd: &cwd,
            git_branch: None,
            date: "2026-09-09",
            mode: Mode::Build,
            plan: &plan,
            instructions: None,
            is_subagent: false,
            skills: &skills,
            skills_enabled: true,
            max_listed_skills: 2,
        };
        let out = system_dynamic(&ctx);
        assert!(out.contains("## Skills\n"));
        assert!(out.contains("- skill-a: Description for skill A\n"));
        // skill-b description must be capped at 200 chars
        assert!(out.contains(&format!("- skill-b: {}\n", "x".repeat(200))));
        assert!(!out.contains(&"x".repeat(201)));
        // skill-c should not be present because max_listed_skills is 2
        assert!(!out.contains("- skill-c"));

        // 2. Skills disabled -> emits NOTHING about skills
        let ctx_disabled = DynamicContext {
            cwd: &cwd,
            git_branch: None,
            date: "2026-09-09",
            mode: Mode::Build,
            plan: &plan,
            instructions: None,
            is_subagent: false,
            skills: &skills,
            skills_enabled: false,
            max_listed_skills: 10,
        };
        let out_disabled = system_dynamic(&ctx_disabled);
        assert!(!out_disabled.contains("## Skills"));
        assert!(!out_disabled.contains("skill-a"));

        // 3. Skills empty -> emits NOTHING about skills
        let ctx_empty = DynamicContext {
            cwd: &cwd,
            git_branch: None,
            date: "2026-09-09",
            mode: Mode::Build,
            plan: &plan,
            instructions: None,
            is_subagent: false,
            skills: &[],
            skills_enabled: true,
            max_listed_skills: 10,
        };
        let out_empty = system_dynamic(&ctx_empty);
        assert!(!out_empty.contains("## Skills"));
    }
}
