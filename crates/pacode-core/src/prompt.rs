//! System prompt (spec §6.4). Static part first (prefix cache), dynamic part second.

use std::path::Path;

pub mod shared;
pub use shared::{PromptOpts, shared};

use pacode_provider::Provider;
use pacode_types::{Mode, Plan, ToolDefinition};

/// Tools listed in the `## Tools` section before the "... and N more" marker.
pub const MAX_LISTED_TOOLS: usize = 64;
/// Total size cap of the `## Tools` section lines (spec §6.4).
pub const MAX_TOOLS_SECTION_CHARS: usize = 6000;
/// Cap of one tool's one-line summary.
const TOOL_SUMMARY_CHARS: usize = 120;

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
    /// The exact tool definitions sent with this request, in request order:
    /// the `## Tools` inventory is rendered from them so it cannot disagree
    /// with the schemas.
    pub tools: &'a [ToolDefinition],
    /// The provider the request goes to: the inventory names each tool the
    /// way this provider sends it on the wire ([`Provider::wire_tool_name`]).
    pub provider: &'a dyn Provider,
}

/// Identity, tool rules, the background-first thesis, plan rules, permission notes.
/// Must be byte-stable across turns of a session.
pub fn system_static() -> &'static str {
    "You are pacode, an autonomous CLI coding agent built around background work.\n\n\
    ## Tool Semantics\n\
    You have access to tools to inspect, modify, and manage the workspace. Use tools to execute tasks directly rather than giving instructions whenever possible.\n\n\
    ## Self-Knowledge\n\
    The `## Tools` section below lists every tool you have. Answer questions about your own capabilities, tools, model or configuration from this prompt alone: never read files, search the repository, or inspect pacode's source or config to answer them; if the prompt does not say, say you do not know. A tool that is not listed is one you do not have: say so plainly and, when sensible, offer a listed tool that can do the job.\n\n\
    ## Large Files\n\
    `read` returns a big source file as an outline: every declaration's signature, the first lines of its body and its closing line, with `… N lines hidden (La-Lb) …` markers in between. Hidden lines carry no anchors and do not count as read: before editing code inside a hidden region, show it with `read` `symbol:\"name\"` or `expand:[\"a-b\"]`. Use `mode:\"full\"` (or offset/limit) for the plain text.\n\n\
    ## Background-First Thesis\n\
    When starting long-running builds, tests, or servers, run them in the background. Continue executing other steps while background tasks run. You will receive task completions and output via injected events between steps.\n\n\
    ## Subagents\n\
    Use the `agent` tool to run independent work in parallel. `spawn` returns immediately; the subagent's final report and its `report_status` updates arrive as `<agent_finished>` / `<agent_status>` events between your steps. Use `agent ask_status` to request a brief status from a running subagent; if you need the result before continuing, end your turn and the report will wake you. `agent send` messages a subagent; an `<agent_question>` blocks its subagent until you answer it with `agent send`.\n\n\
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
    // Subagents do not own the plan or the mode; omitting both keeps their system
    // prefix byte-stable while the orchestrator changes them (provider prefix cache).
    if !ctx.is_subagent {
        out.push_str(&format!("- Mode: {}\n", ctx.mode.as_str()));
    }

    if ctx.is_subagent {
        out.push_str("\n## Subagent Role\n");
        out.push_str(&subagent_role(ctx));
    }

    if !ctx.is_subagent && !ctx.plan.is_empty() {
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

    out.push_str("\n## Tools\n");
    out.push_str(&tools_section(ctx.tools, ctx.provider));
    out
}

/// The subagent's role. The channel tools are named only when the subagent
/// has them (a kind or `tools` list may narrow them away; telling the model
/// to call a tool it lacks costs a failed call), and by their wire names.
fn subagent_role(ctx: &DynamicContext<'_>) -> String {
    let named = |name: &str| {
        ctx.tools
            .iter()
            .any(|tool| tool.name == name)
            .then(|| ctx.provider.wire_tool_name(name).into_owned())
    };
    let mut role = String::from(
        "You are a subagent working on a dedicated subtask. The `agent` tool is not available \
         to you. Focus on completing your assigned prompt and return your findings or results.",
    );
    if let Some(report) = named("report_status") {
        role.push_str(&format!(
            " When you receive a `<status_request>`, call `{report}` with a brief status \
             first, then continue."
        ));
    }
    role.push_str(
        " An `<orchestrator_message>` is an instruction from the orchestrator; where it \
         conflicts with your original brief, it wins.",
    );
    if let Some(ask) = named("ask_parent") {
        role.push_str(&format!(
            " Use `{ask}` only when you are genuinely blocked on a decision the orchestrator \
             or the user must make; otherwise proceed on a reasonable assumption and state it \
             in your report — do not ping-pong."
        ));
    }
    role.push_str(" Your final message is delivered to the orchestrator as your report.\n");
    role
}

/// One line per tool, `- name: first sentence`, capped at [`MAX_LISTED_TOOLS`]
/// lines and [`MAX_TOOLS_SECTION_CHARS`] chars with a visible marker for the rest.
/// `name` is the wire name `provider` sends in the schemas.
fn tools_section(tools: &[ToolDefinition], provider: &dyn Provider) -> String {
    if tools.is_empty() {
        return "(none)\n".to_string();
    }
    let mut out = String::new();
    let mut chars = 0;
    for (listed, tool) in tools.iter().enumerate() {
        let name = provider.wire_tool_name(&tool.name);
        let line = format!("- {name}: {}\n", tool_summary(&tool.description));
        let line_chars = line.chars().count();
        if listed == MAX_LISTED_TOOLS || chars + line_chars > MAX_TOOLS_SECTION_CHARS {
            let rest = tools.len() - listed;
            out.push_str(&format!(
                "- ... and {rest} more not listed here; check the attached tool schemas before saying a tool is missing.\n"
            ));
            break;
        }
        chars += line_chars;
        out.push_str(&line);
    }
    out
}

/// First sentence of the first paragraph, whitespace flattened to one line
/// (descriptions wrap mid-sentence), capped at [`TOOL_SUMMARY_CHARS`].
fn tool_summary(description: &str) -> String {
    let paragraph = description
        .split("\n\n")
        .find(|p| !p.trim().is_empty())
        .unwrap_or("");
    let line = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    // A ". " after "e.g" / "i.e" is not a sentence end.
    let end = line
        .match_indices(". ")
        .map(|(i, _)| i)
        .find(|&i| !line[..i].ends_with("e.g") && !line[..i].ends_with("i.e"));
    let sentence = match end {
        Some(end) => &line[..=end],
        None => line.as_str(),
    };
    if sentence.chars().count() <= TOOL_SUMMARY_CHARS {
        return sentence.to_string();
    }
    let cut: String = sentence.chars().take(TOOL_SUMMARY_CHARS - 3).collect();
    // Back off to the last word boundary so the line never ends mid-word; a
    // single word longer than the cap is cut hard.
    let cut = match cut.rfind(char::is_whitespace) {
        Some(space) => &cut[..space],
        None => cut.as_str(),
    };
    format!("{}...", cut.trim_end_matches([' ', ',', ';', ':']))
}

pub const DEFAULT_MEMORY_CAP_CHARS: usize = 8000;

/// Memory section of the system prompt, built from the global (`<config dir>/memory.db`)
/// and project (`<cwd>/.pacode/memory.db`) stores: every pinned entry (20 per scope), a
/// one-line index (title + id) of the 10 most recently updated others per scope, and a
/// sentence pointing the model at `memory_recall` / `memory_write`. Capped at
/// `memory_cap_chars` (index lines are dropped first). Output is deterministic for an
/// unchanged store, which keeps the prompt prefix cache-friendly. Scopes without a
/// database (or a legacy `memory.md`, imported on first open) are skipped and nothing
/// is created on disk; `None` when there is nothing to show.
pub fn load_memory(
    cwd: &Path,
    home_config: Option<&Path>,
    memory_cap_chars: usize,
) -> Option<String> {
    if memory_cap_chars == 0 {
        return None;
    }
    let global_dir = home_config
        .map(Path::to_path_buf)
        .or_else(|| {
            pacode_config::Paths::discover()
                .memory_file()
                .parent()
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| std::path::PathBuf::from(".pacode"));
    let store = pacode_tools::builtin::memory::store_for_dirs(&global_dir, cwd);
    pacode_memory::render_prompt_section(&store.snapshot(), memory_cap_chars)
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

/// Variant of `load_instructions` with explicit memory cap chars. Foreign-agent
/// instruction files are discovered (see [`load_instructions_with_options`]).
pub fn load_instructions_with_memory(
    cwd: &Path,
    home_config: Option<&Path>,
    instructions_cap_chars: usize,
    memory_cap_chars: usize,
) -> Option<String> {
    load_instructions_with_options(
        cwd,
        home_config,
        instructions_cap_chars,
        memory_cap_chars,
        true,
    )
}

/// Full variant. `discover_foreign` mirrors `[context] discover_foreign_instructions`: when
/// true, instruction files written for other agents (`.cursor/rules`, `.clinerules`, Copilot,
/// Windsurf, Gemini, Codex) found in `cwd` and its ancestors are appended after the files above.
pub fn load_instructions_with_options(
    cwd: &Path,
    home_config: Option<&Path>,
    instructions_cap_chars: usize,
    memory_cap_chars: usize,
    discover_foreign: bool,
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

    if discover_foreign {
        let foreign = pacode_import::instructions::discover_instructions(cwd);
        let section = pacode_import::instructions::render_prompt_section(&foreign, cwd);
        if !section.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n\n");
            }
            combined.push_str(&section);
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
/// Obtained at turn start through [`shared`] (one `Arc` per cwd, shared by all agents; see
/// `prompt/shared.rs` for cap, TTL and invalidation) and reused across all steps of a
/// single turn to preserve provider prefix caching and avoid syscalls.
#[derive(Clone, Debug)]
pub struct TurnPromptCache {
    pub git_branch: Option<String>,
    pub instructions: Option<String>,
    pub date: String,
}

impl TurnPromptCache {
    /// Last-resort value (a failed blocking build): no branch, no instructions.
    pub fn empty() -> Self {
        Self {
            git_branch: None,
            instructions: None,
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        }
    }

    /// Invalidation point: compute fresh values at turn start.
    pub fn new(
        cwd: &Path,
        home_config: Option<&Path>,
        instructions_cap_chars: usize,
        memory_cap_chars: usize,
    ) -> Self {
        Self::with_options(
            cwd,
            home_config,
            instructions_cap_chars,
            memory_cap_chars,
            true,
        )
    }

    /// `new` with the foreign-instruction switch (`[context] discover_foreign_instructions`).
    pub fn with_options(
        cwd: &Path,
        home_config: Option<&Path>,
        instructions_cap_chars: usize,
        memory_cap_chars: usize,
        discover_foreign: bool,
    ) -> Self {
        let git_branch = git_branch(cwd);
        let instructions = load_instructions_with_options(
            cwd,
            home_config,
            instructions_cap_chars,
            memory_cap_chars,
            discover_foreign,
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
