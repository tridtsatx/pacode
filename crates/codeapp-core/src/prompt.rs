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
    todo!("prompt::system_static")
}

pub fn system_dynamic(ctx: &DynamicContext<'_>) -> String {
    let _ = ctx;
    todo!("prompt::system_dynamic")
}

/// Collect `AGENTS.md` and `CLAUDE.md` from `cwd` up to the filesystem root (nearest
/// last so it wins), plus `~/.config/codeapp/AGENTS.md` first. Each file is prefixed
/// with `# <path>`; the total is capped to `cap_chars` (head+tail).
pub fn load_instructions(
    cwd: &Path,
    home_config: Option<&Path>,
    cap_chars: usize,
) -> Option<String> {
    let _ = (cwd, home_config, cap_chars);
    todo!("prompt::load_instructions")
}

/// `git rev-parse --abbrev-ref HEAD` equivalent by reading `.git/HEAD` (no process).
pub fn git_branch(cwd: &Path) -> Option<String> {
    let _ = cwd;
    todo!("prompt::git_branch")
}
