use std::collections::{HashSet, VecDeque};

use super::*;

fn ids(names: &[&str]) -> VecDeque<PacodeSessionId> {
    names.iter().map(|n| PacodeSessionId::new(*n)).collect()
}

fn names(order: &VecDeque<PacodeSessionId>) -> Vec<String> {
    order.iter().map(|id| id.to_string()).collect()
}

#[test]
fn evicts_the_least_recently_used_session() {
    let order = ids(&["a", "b", "c"]);
    assert_eq!(evict_candidate(&order, |_| false), Some(0));
}

#[test]
fn skips_a_session_with_a_turn_in_flight() {
    let order = ids(&["a", "b", "c"]);
    let busy: HashSet<String> = ["a".to_string()].into_iter().collect();
    // `a` is the oldest but is mid-turn, so `b` is dropped instead.
    assert_eq!(
        evict_candidate(&order, |id| busy.contains(&id.to_string())),
        Some(1)
    );
}

#[test]
fn refuses_to_evict_when_every_session_is_busy() {
    let order = ids(&["a", "b"]);
    // Going one over the cap beats cutting off a running turn.
    assert_eq!(evict_candidate(&order, |_| true), None);
}

#[test]
fn evict_candidate_of_an_empty_queue_is_none() {
    assert_eq!(evict_candidate(&VecDeque::new(), |_| false), None);
}

#[test]
fn touch_moves_a_session_to_the_most_recent_end() {
    let mut order = ids(&["a", "b", "c"]);
    touch_order(&mut order, &PacodeSessionId::new("a"));
    assert_eq!(names(&order), ["b", "c", "a"]);
}

#[test]
fn touching_the_newest_session_keeps_the_order() {
    let mut order = ids(&["a", "b", "c"]);
    touch_order(&mut order, &PacodeSessionId::new("c"));
    assert_eq!(names(&order), ["a", "b", "c"]);
}

#[test]
fn touching_an_unknown_session_changes_nothing() {
    let mut order = ids(&["a", "b"]);
    touch_order(&mut order, &PacodeSessionId::new("zzz"));
    assert_eq!(names(&order), ["a", "b"]);
}

#[test]
fn a_touched_session_is_no_longer_the_eviction_candidate() {
    let mut order = ids(&["a", "b", "c"]);
    touch_order(&mut order, &PacodeSessionId::new("a"));
    // `a` was the oldest by creation; after use, `b` is the one to drop.
    assert_eq!(evict_candidate(&order, |_| false), Some(0));
    assert_eq!(
        order.front().map(|id| id.to_string()),
        Some("b".to_string())
    );
}
