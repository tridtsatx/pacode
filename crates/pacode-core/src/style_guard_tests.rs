use pacode_rules::{Finding, Severity};
use pacode_types::SessionId;

use super::*;

fn finding(rule: &str, max: u32) -> Finding {
    Finding {
        rule: rule.to_string(),
        severity: Severity::Warn,
        line: 1,
        snippet: "x".to_string(),
        message: format!("message of {rule}"),
        max_per_turn: max,
    }
}

#[test]
fn limiter_resets_only_when_the_turn_marker_changes() {
    let mut guard = SessionGuard::default();
    assert_eq!(
        guard
            .limiter_for("main", 1)
            .admit(vec![finding("a", 1)])
            .len(),
        1
    );
    // same turn: budget spent
    assert!(
        guard
            .limiter_for("main", 1)
            .admit(vec![finding("a", 1)])
            .is_empty()
    );
    // another agent has its own budget
    assert_eq!(
        guard
            .limiter_for("sub", 1)
            .admit(vec![finding("a", 1)])
            .len(),
        1
    );
    // new user message: fresh budget
    assert_eq!(
        guard
            .limiter_for("main", 2)
            .admit(vec![finding("a", 1)])
            .len(),
        1
    );
}

#[test]
fn agent_table_is_capped() {
    let mut guard = SessionGuard::default();
    for i in 0..(MAX_AGENTS_PER_SESSION + 5) {
        guard.limiter_for(&format!("agent-{i}"), 1);
        assert!(guard.agents.len() <= MAX_AGENTS_PER_SESSION);
    }
}

#[test]
fn pins_one_line_per_rule_and_caps_the_list() {
    let mut guard = SessionGuard::default();
    guard.pin(&[finding("a", 3), finding("a", 3), finding("b", 3)]);
    assert_eq!(guard.pinned.len(), 2);
    assert!(guard.pinned[0].starts_with("[style-guard] a:"));
    guard.pin(&[finding("a", 3)]);
    assert_eq!(guard.pinned.len(), 2);

    for i in 0..(MAX_PINNED + 10) {
        guard.pin(&[finding(&format!("rule-{i}"), 3)]);
    }
    assert_eq!(guard.pinned.len(), MAX_PINNED);
    assert!(
        guard
            .pinned
            .last()
            .is_some_and(|l| l.contains(&format!("rule-{}", MAX_PINNED + 9)))
    );
    assert!(
        !guard
            .pinned
            .iter()
            .any(|l| l.starts_with("[style-guard] a:"))
    );
}

/// One test on purpose: the registry is process-wide, so the LRU eviction part must
/// not run concurrently with the pinned-reminder part.
#[test]
fn registry_pins_forgets_and_evicts_lru() {
    let id = SessionId::new("pacode-style-guard-test-pinned");
    let key = id.to_string();
    with_session(&key, |g| g.pin(&[finding("keep-me", 3)]));
    let pinned = pinned_reminders(&id);
    assert_eq!(pinned.len(), 1);
    assert!(pinned[0].contains("keep-me"));
    forget_session(&id);
    assert!(pinned_reminders(&id).is_empty());

    let ids: Vec<String> = (0..MAX_SESSIONS + 3)
        .map(|i| format!("lru-test-{i}"))
        .collect();
    for id in &ids {
        with_session(id, |g| g.pin(&[finding("r", 1)]));
    }
    let len = REGISTRY
        .get()
        .map(|r| r.lock().unwrap_or_else(|p| p.into_inner()).sessions.len())
        .unwrap_or(0);
    assert!(len <= MAX_SESSIONS, "registry grew to {len}");
    for id in &ids {
        forget_session(&SessionId::new(id.clone()));
    }
}
