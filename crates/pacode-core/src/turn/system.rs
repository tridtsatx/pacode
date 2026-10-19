//! Per-turn system prompt assembly (perf C3/C4).

use std::path::Path;
use std::sync::Arc;

use crate::agent::Agent;
use crate::prompt::{DynamicContext, PromptOpts, TurnPromptCache};
use crate::session::Session;

/// Shared prompt cache for `cwd` (see `prompt/shared.rs`): a main turn start invalidates it.
pub(super) async fn turn_prompt_cache(
    session: &Session,
    agent: &Agent,
    cwd: &Path,
) -> Arc<TurnPromptCache> {
    let memory_cap_chars = if session.config.memory.enabled {
        session.config.context.memory_cap_chars
    } else {
        0
    };
    let opts = PromptOpts {
        home_config: pacode_config::Paths::discover()
            .config_file
            .parent()
            .map(|d| d.to_path_buf()),
        instructions_cap_chars: session.config.context.instructions_cap_chars,
        memory_cap_chars,
        discover_foreign: session.config.context.discover_foreign_instructions,
    };
    crate::prompt::shared(cwd, opts, agent.id().is_main()).await
}

/// The dynamic system text of one turn: `(plan.version, text)`, local to `run_turn`.
/// Rebuilt only when the plan version changes; subagent prompts do not contain the plan,
/// so theirs is built once per turn (key `0`).
pub(super) type SystemSlot = Option<(u64, Arc<str>)>;

pub(super) fn dynamic(
    slot: &mut SystemSlot,
    session: &Session,
    agent: &Agent,
    meta: &pacode_types::SessionMeta,
    cache: &TurnPromptCache,
) -> Arc<str> {
    let is_subagent = !agent.id().is_main();
    let plan_guard = session.plan.read().unwrap_or_else(|p| p.into_inner());
    let key = if is_subagent { 0 } else { plan_guard.version };
    if let Some((k, text)) = slot.as_ref()
        && *k == key
    {
        return text.clone();
    }
    let dyn_ctx = DynamicContext {
        cwd: &meta.cwd,
        git_branch: cache.git_branch.as_deref(),
        date: &cache.date,
        mode: meta.mode,
        plan: &plan_guard,
        instructions: cache.instructions.as_deref(),
        is_subagent,
        skills: session.skills.skills(),
        skills_enabled: session.config.skills.enabled,
        max_listed_skills: session.config.skills.max_listed,
    };
    let text: Arc<str> = Arc::from(crate::prompt::system_dynamic(&dyn_ctx));
    *slot = Some((key, text.clone()));
    text
}
