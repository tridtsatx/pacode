//! Code-style guard: runs the `pacode-rules` rule set over what write/edit tool
//! calls add, and tells the model when it broke a rule.
//!
//! Hook points:
//! - `turn/tools.rs`: [`snapshot`] before an edit/write tool runs, [`check_after_write`]
//!   after it succeeded (appends a `<style-guard>` block to the tool result).
//! - `turn/stream.rs`: [`StreamGuard::check`] while write/edit arguments stream in; a
//!   hit cuts the call short and the model gets the rule as the tool error.
//!
//! State (all in one process-wide registry, see [`with_session`]):
//! - rule set cache, one per session. Cap: 1 set. Invalidation: the rule files'
//!   (path, mtime, size) fingerprint or the `[rules]` config changed. Owner: registry.
//! - per-agent [`TurnLimiter`]. Cap: [`MAX_AGENTS_PER_SESSION`]. Invalidation: reset
//!   when the agent's latest real user message changes (a new turn). Owner: registry.
//! - pinned reminders, one line per rule that fired. Cap: [`MAX_PINNED`] (oldest
//!   dropped). Read by [`pinned_reminders`] for the compaction pinned-blocks hook.
//! - the registry itself. Cap: [`MAX_SESSIONS`], least recently used evicted;
//!   [`forget_session`] drops one eagerly. Nothing here runs on a timer.

mod after_write;
mod stream;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use pacode_rules::{Finding, Fingerprint, LoadOptions, RuleSet, TurnLimiter, pinned_line};
use pacode_types::{
    ContentBlock, Event, MessageKind, Role, SessionId, ToastLevel, TranscriptItem, TranscriptKind,
};

use crate::agent::Agent;
use crate::session::Session;

pub(crate) use after_write::{check_after_write, snapshot};
pub(crate) use stream::StreamGuard;

/// Pinned reminders kept per session.
pub const MAX_PINNED: usize = 32;
/// Sessions tracked at once.
pub const MAX_SESSIONS: usize = 64;
/// Agents (main + subagents) tracked per session.
pub const MAX_AGENTS_PER_SESSION: usize = 32;

#[derive(Default)]
struct AgentTurn {
    /// Identity of the user message that started the current turn.
    marker: usize,
    limiter: TurnLimiter,
}

#[derive(Default)]
pub(crate) struct SessionGuard {
    ruleset: Option<(Fingerprint, Arc<RuleSet>)>,
    pinned: Vec<String>,
    agents: BTreeMap<String, AgentTurn>,
    last_used: u64,
}

impl SessionGuard {
    /// The limiter of `agent_key`, reset when `marker` shows a new turn started.
    fn limiter_for(&mut self, agent_key: &str, marker: usize) -> &mut TurnLimiter {
        if self.agents.len() >= MAX_AGENTS_PER_SESSION && !self.agents.contains_key(agent_key) {
            self.agents.clear();
        }
        let turn = self.agents.entry(agent_key.to_string()).or_default();
        if turn.marker != marker {
            turn.marker = marker;
            turn.limiter.reset();
        }
        &mut turn.limiter
    }

    /// Pin one line per rule (first message wins), dropping the oldest past the cap.
    fn pin(&mut self, findings: &[Finding]) {
        for f in findings {
            let prefix = format!("[style-guard] {}:", f.rule);
            if self.pinned.iter().any(|p| p.starts_with(&prefix)) {
                continue;
            }
            self.pinned.push(pinned_line(f));
            if self.pinned.len() > MAX_PINNED {
                self.pinned.remove(0);
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    sessions: BTreeMap<String, SessionGuard>,
    tick: u64,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

pub(crate) fn with_session<R>(session_id: &str, f: impl FnOnce(&mut SessionGuard) -> R) -> R {
    let registry = REGISTRY.get_or_init(|| Mutex::new(Registry::default()));
    let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
    reg.tick += 1;
    let tick = reg.tick;
    if !reg.sessions.contains_key(session_id) && reg.sessions.len() >= MAX_SESSIONS {
        let oldest = reg
            .sessions
            .iter()
            .min_by_key(|(_, s)| s.last_used)
            .map(|(k, _)| k.clone());
        if let Some(key) = oldest {
            reg.sessions.remove(&key);
        }
    }
    let entry = reg.sessions.entry(session_id.to_string()).or_default();
    entry.last_used = tick;
    f(entry)
}

/// Reminder lines that must survive compaction (one per rule that fired in this
/// session, at most [`MAX_PINNED`]). For the compaction pinned-blocks hook.
pub fn pinned_reminders(session_id: &SessionId) -> Vec<String> {
    let Some(registry) = REGISTRY.get() else {
        return Vec::new();
    };
    let reg = registry.lock().unwrap_or_else(|p| p.into_inner());
    reg.sessions
        .get(&session_id.to_string())
        .map(|g| g.pinned.clone())
        .unwrap_or_default()
}

/// Drop all guard state of a closed session.
pub fn forget_session(session_id: &SessionId) {
    if let Some(registry) = REGISTRY.get() {
        let mut reg = registry.lock().unwrap_or_else(|p| p.into_inner());
        reg.sessions.remove(&session_id.to_string());
    }
}

fn load_options(session: &Session) -> LoadOptions {
    let cfg = &session.config.rules;
    let global_dir = pacode_config::Paths::discover()
        .config_file
        .parent()
        .map(|p| p.join("rules"));
    LoadOptions {
        builtin: cfg.builtin,
        disable: cfg.disable.clone(),
        project_dir: Some(session.meta().cwd.join(".pacode").join("rules")),
        global_dir,
    }
}

/// The session's rule set, reloaded only when the rule files or `[rules]` changed.
/// `None` when the guard is disabled or the set is empty.
pub(crate) async fn ruleset_for(session: &Session) -> Option<Arc<RuleSet>> {
    if !session.config.rules.enabled {
        return None;
    }
    let sid = session.id.to_string();
    let opts = load_options(session);
    let cached = with_session(&sid, |g| g.ruleset.clone());
    let cached_fp = cached.as_ref().map(|(fp, _)| fp.clone());
    let reloaded = tokio::task::spawn_blocking(move || {
        let fp = opts.fingerprint();
        if cached_fp.as_ref() == Some(&fp) {
            None
        } else {
            Some((fp, RuleSet::load(&opts)))
        }
    })
    .await
    .ok()?;
    let set = match reloaded {
        None => cached.map(|(_, set)| set)?,
        Some((fp, loaded)) => {
            for err in &loaded.errors {
                log::warn!("style-guard rule skipped: {err}");
            }
            let set = Arc::new(loaded.set);
            with_session(&sid, |g| g.ruleset = Some((fp, set.clone())));
            set
        }
    };
    (!set.is_empty()).then_some(set)
}

/// Identity of the agent's latest real user message (a turn starts with one). Tool
/// results, injections and compaction summaries do not count.
fn turn_marker(agent: &Agent) -> usize {
    let history = agent.history.lock().unwrap_or_else(|p| p.into_inner());
    for msg in history.messages.iter().rev() {
        let is_user =
            matches!(msg.role, Role::User) && matches!(msg.meta.kind, MessageKind::Normal);
        let has_text = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. }));
        let has_result = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
        if is_user && has_text && !has_result {
            return Arc::as_ptr(msg) as *const () as usize;
        }
    }
    0
}

/// Apply the per-turn limits to fresh findings and pin what remains. `stream`
/// findings also spend one of the turn's stream retries.
pub(crate) fn admit(
    session: &Session,
    agent: &Agent,
    findings: Vec<Finding>,
    stream: bool,
) -> Vec<Finding> {
    let marker = turn_marker(agent);
    let agent_key = agent.id().to_string();
    with_session(session.id.as_ref(), |g| {
        let limiter = g.limiter_for(&agent_key, marker);
        let kept = if stream {
            limiter.admit_stream(findings)
        } else {
            limiter.admit(findings)
        };
        g.pin(&kept);
        kept
    })
}

/// Transcript note `style-guard: <rule> fired` per distinct rule.
pub(crate) fn emit_fired(session: &Session, agent: &Agent, findings: &[Finding]) {
    let mut seen: Vec<&str> = Vec::new();
    for f in findings {
        if seen.contains(&f.rule.as_str()) {
            continue;
        }
        seen.push(&f.rule);
        let seq = agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .next_seq();
        let item = TranscriptItem {
            seq,
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::Notice {
                level: ToastLevel::Warn,
                text: format!("style-guard: {} fired", f.rule),
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemAdded(item));
    }
}

#[cfg(test)]
#[path = "style_guard_tests.rs"]
mod style_guard_tests;
