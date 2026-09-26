use std::path::Path;

use pacode_types::SessionId;

use super::*;

#[test]
fn controllers_are_matched_exactly_not_by_substring() {
    let list = "cpuset cpu io memory hugetlb pids rdma misc dmem";
    assert!(controller_enabled(list, "memory"));
    assert!(!controller_enabled(list, "mem"));
    // `memory_recursive` must not be taken for `memory`: enabling the wrong
    // controller would silently give the scope no limit at all.
    assert!(!controller_enabled("memory_recursive", "memory"));
    assert!(!controller_enabled("", "memory"));
    assert!(!controller_enabled("  \n ", "memory"));
}

#[test]
fn a_group_name_cannot_escape_its_scope() {
    let scope = Path::new("/sys/fs/cgroup/user.slice/pacode-exec-1");
    for hostile in ["../../etc", "/etc/passwd", "..", "a/b", ".hidden", ""] {
        let name = group_name(&SessionId::new(hostile));
        let joined = scope.join(&name);
        assert_eq!(
            joined.parent(),
            Some(scope),
            "{hostile:?} produced a name that leaves the scope"
        );
        assert!(!name.contains('/'), "{hostile:?} -> {name}");
        assert!(!name.starts_with('.'), "{hostile:?} -> {name}");
    }
}

#[test]
fn a_generated_session_id_keeps_its_shape() {
    let id = SessionId::generate();
    let name = group_name(&id);
    assert_eq!(name, format!("s-{}", id.as_str()));
    assert!(!name.contains('/'));
}

#[test]
fn two_ids_that_escape_alike_still_get_different_groups() {
    // Both escape to the same sanitised stem, so the tail has to separate them
    // or one session would inherit another's group — and its limit.
    let first = group_name(&SessionId::new("a/b"));
    let second = group_name(&SessionId::new("a:b"));
    assert_ne!(first, second);
    assert!(first.len() <= 57, "{}", first.len());
    assert!(second.len() <= 57, "{}", second.len());
}

#[test]
fn a_granted_raise_cannot_push_past_the_scope_ceiling() {
    const SCOPE_MAX: u64 = 80 * 1024 * 1024 * 1024;
    // One session asking for more may not spend what the others are holding.
    assert_eq!(
        clamp_to_scope(90 * 1024 * 1024 * 1024, SCOPE_MAX),
        SCOPE_MAX
    );
    assert_eq!(clamp_to_scope(1, SCOPE_MAX), 1);
    assert_eq!(
        clamp_to_scope(60 * 1024 * 1024 * 1024, SCOPE_MAX),
        60 * 1024 * 1024 * 1024
    );
}
