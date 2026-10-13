//! Test-only hooks into the window around a finishing subagent's seal, so a
//! test can land an `agent send` exactly before or after it. Keyed by agent
//! id (ids are unique per test), each hook runs once.

use std::sync::{Mutex, OnceLock};

use pacode_types::AgentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    /// The turn decided it owes nothing and is about to seal.
    BeforeSeal,
    /// The turn sealed and is about to finalize (report, drop its history).
    AfterSeal,
}

type Hook = Box<dyn FnOnce() + Send>;

fn hooks() -> &'static Mutex<Vec<(AgentId, Point, Hook)>> {
    static HOOKS: OnceLock<Mutex<Vec<(AgentId, Point, Hook)>>> = OnceLock::new();
    HOOKS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn install(agent: AgentId, point: Point, hook: impl FnOnce() + Send + 'static) {
    hooks()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((agent, point, Box::new(hook)));
}

pub(crate) fn run(agent: &AgentId, point: Point) {
    let hook = {
        let mut hooks = hooks().lock().unwrap_or_else(|p| p.into_inner());
        let index = hooks
            .iter()
            .position(|(id, at, _)| id == agent && *at == point);
        index.map(|index| hooks.remove(index).2)
    };
    if let Some(hook) = hook {
        hook();
    }
}
