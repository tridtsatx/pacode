//! Test-only hooks into the windows of the exchange — around a finishing
//! subagent's seal, between a question's lookup and its queueing, and between
//! a resume's last await and the point where it takes effect — so a test can
//! land a send, a stop, a close or a detach exactly there. Keyed by agent id
//! (ids are unique per test), each hook runs once.

use std::sync::{Mutex, OnceLock};

use pacode_types::AgentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    /// The turn decided it owes nothing and is about to seal.
    BeforeSeal,
    /// The turn sealed and is about to finalize (report, drop its history).
    AfterSeal,
    /// `ask_parent` found the parent and is about to queue the question.
    AskLookedUp,
    /// A resume passed its checks and is about to take the message lock.
    ResumeChecked,
    /// A resume persisted the message and is about to take effect.
    ResumeCommitted,
    /// `continue` persisted its nudge and is about to take effect.
    ContinueCommitted,
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

/// Run the other side of a race on a thread of its own: a task spawned from
/// the hook would sit in the blocked worker's LIFO slot, which no other worker
/// steals, and never run while the hook waits for it.
pub(crate) fn race<F>(fut: F) -> std::thread::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || runtime.block_on(fut))
}

/// Block the hook's thread until `cond` holds — the other side of the race
/// runs on its own thread meanwhile. Panics after `secs`.
pub(crate) fn block_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
