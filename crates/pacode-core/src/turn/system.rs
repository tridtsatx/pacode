//! Per-turn system prompt inputs (perf C3): the shared prompt cache. The dynamic
//! part is rendered per request in `run_turn` (it names the request's tools as its
//! provider sends them).

use std::path::Path;
use std::sync::Arc;

use crate::agent::Agent;
use crate::prompt::{PromptOpts, TurnPromptCache};
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
