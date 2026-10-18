use pretty_assertions::assert_eq;

use crate::{Finding, MAX_STREAM_RETRIES_PER_TURN, Severity, TurnLimiter};

fn finding(rule: &str, max: u32) -> Finding {
    Finding {
        rule: rule.to_string(),
        severity: Severity::Warn,
        line: 1,
        snippet: String::new(),
        message: "m".to_string(),
        max_per_turn: max,
    }
}

#[test]
fn max_per_turn_is_per_rule_and_per_firing() {
    let mut l = TurnLimiter::default();
    // two hits of one rule in one call = one firing
    assert_eq!(l.admit(vec![finding("a", 2), finding("a", 2)]).len(), 2);
    assert_eq!(l.fired("a"), 1);
    assert_eq!(l.admit(vec![finding("a", 2), finding("b", 1)]).len(), 2);
    assert_eq!(l.fired("a"), 2);
    // a is exhausted, b is exhausted
    assert!(l.admit(vec![finding("a", 2), finding("b", 1)]).is_empty());
    // a new rule still fires
    assert_eq!(l.admit(vec![finding("c", 1)]).len(), 1);
}

#[test]
fn stream_retries_are_capped_at_two_per_turn() {
    let mut l = TurnLimiter::default();
    assert_eq!(MAX_STREAM_RETRIES_PER_TURN, 2);
    assert_eq!(l.admit_stream(vec![finding("a", 5)]).len(), 1);
    assert_eq!(l.admit_stream(vec![finding("b", 5)]).len(), 1);
    assert_eq!(l.retries(), 2);
    // retries spent: nothing returned and nothing consumed, so after_write still fires
    assert!(l.admit_stream(vec![finding("c", 5)]).is_empty());
    assert_eq!(l.fired("c"), 0);
    assert_eq!(l.admit(vec![finding("c", 5)]).len(), 1);
}

#[test]
fn same_rule_cannot_loop_in_stream() {
    let mut l = TurnLimiter::default();
    assert_eq!(l.admit_stream(vec![finding("a", 1)]).len(), 1);
    assert!(l.admit_stream(vec![finding("a", 1)]).is_empty());
    // exhausted rule does not burn a retry
    assert_eq!(l.retries(), 1);
}

#[test]
fn reset_starts_a_new_turn() {
    let mut l = TurnLimiter::default();
    l.admit_stream(vec![finding("a", 1)]);
    l.reset();
    assert_eq!(l.retries(), 0);
    assert_eq!(l.fired("a"), 0);
    assert_eq!(l.admit(vec![finding("a", 1)]).len(), 1);
}
