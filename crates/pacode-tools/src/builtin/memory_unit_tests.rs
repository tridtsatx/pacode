use pacode_memory::MemoryStore;
use serde_json::json;

use super::*;

fn store() -> (tempfile::TempDir, MemoryStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let s = MemoryStore::for_dirs(&dir.path().join("cfg"), &dir.path().join("proj"));
    (dir, s)
}

fn write(s: &MemoryStore, v: serde_json::Value) -> Result<String, ToolError> {
    run_write(s, serde_json::from_value(v).unwrap(), None)
}

fn recall(s: &MemoryStore, v: serde_json::Value) -> String {
    run_recall(s, serde_json::from_value(v).unwrap()).unwrap()
}

#[test]
fn old_write_shape_still_works() {
    let (_d, s) = store();
    let long = "use rust edition 2024 and keep modules small ".repeat(4);
    let out = write(&s, json!({"scope": "project", "text": long})).unwrap();
    assert!(out.starts_with("stored "), "{out}");
    assert!(out.ends_with("(project)"));
    let entries = s.list(&[pacode_memory::Scope::Project], None, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, pacode_memory::Kind::Fact);
    assert_eq!(entries[0].title.chars().count(), 60);
    assert!(long.trim().starts_with(&entries[0].title));
}

#[test]
fn duplicate_write_reports_updated() {
    let (_d, s) = store();
    let a = write(
        &s,
        json!({"scope": "global", "title": "Style", "text": "prefer concise code"}),
    )
    .unwrap();
    let b = write(
        &s,
        json!({"scope": "global", "title": "style", "text": "prefer very concise code"}),
    )
    .unwrap();
    assert!(a.starts_with("stored "));
    assert!(b.starts_with("updated "));
    assert_eq!(a.split_whitespace().nth(1), b.split_whitespace().nth(1));
}

#[test]
fn recall_alias_and_empty_query_list_both_scopes() {
    let (_d, s) = store();
    write(&s, json!({"scope": "global", "kind": "preference", "title": "Tone", "text": "global knowledge", "pinned": true})).unwrap();
    write(
        &s,
        json!({"scope": "project", "title": "Layout", "text": "project knowledge"}),
    )
    .unwrap();
    // Old `memory_read` call shape: only a scope (or nothing).
    let both = recall(&s, json!({}));
    assert!(both.contains("[global/preference]"));
    assert!(both.contains("[project/fact]"));
    assert!(both.contains("global knowledge") && both.contains("project knowledge"));
    let only = recall(&s, json!({"scope": "project"}));
    assert!(only.contains("project knowledge") && !only.contains("global knowledge"));
    let q = recall(
        &s,
        json!({"query": "knowledge", "scope": "both", "limit": 1}),
    );
    assert!(q.starts_with("1 memories"));
    assert_eq!(
        recall(&s, json!({"query": "nonexistentzzz"})),
        "no memories found"
    );
}

#[test]
fn forget_and_list() {
    let (_d, s) = store();
    let out = write(
        &s,
        json!({"scope": "global", "title": "Temp", "text": "short lived note"}),
    )
    .unwrap();
    let id = out.split_whitespace().nth(1).unwrap().to_string();
    let listed = run_list(&s, serde_json::from_value(json!({})).unwrap()).unwrap();
    assert!(listed.contains(&id) && listed.contains("Temp"));
    assert!(!listed.contains("short lived"), "list shows titles only");
    let f = run_forget(&s, serde_json::from_value(json!({"id": id})).unwrap()).unwrap();
    assert!(f.starts_with("forgot "));
    assert!(run_forget(&s, serde_json::from_value(json!({"id": id})).unwrap()).is_err());
    assert_eq!(
        run_list(&s, serde_json::from_value(json!({})).unwrap()).unwrap(),
        "no memories"
    );
}

#[test]
fn bad_inputs_are_invalid_input_errors() {
    let (_d, s) = store();
    assert!(matches!(
        write(&s, json!({"scope": "nowhere", "text": "x"})),
        Err(ToolError::InvalidInput(_))
    ));
    assert!(matches!(
        write(
            &s,
            json!({"scope": "global", "text": "x", "kind": "opinion"})
        ),
        Err(ToolError::InvalidInput(_))
    ));
    assert!(matches!(
        write(&s, json!({"scope": "global", "text": "   "})),
        Err(ToolError::InvalidInput(_))
    ));
    assert!(run_recall(&s, serde_json::from_value(json!({"scope": "x"})).unwrap()).is_err());
}

#[test]
fn age_formatting() {
    assert_eq!(format_age(0), "just now");
    assert_eq!(format_age(5 * 60_000), "5m ago");
    assert_eq!(format_age(3 * 3_600_000), "3h ago");
    assert_eq!(format_age(2 * 86_400_000), "2d ago");
    assert_eq!(format_age(90 * 86_400_000), "3mo ago");
}

#[test]
fn store_cache_returns_same_instance_per_key() {
    let dir = tempfile::tempdir().unwrap();
    let a = store_for_dirs(&dir.path().join("g"), &dir.path().join("p"));
    let b = store_for_dirs(&dir.path().join("g"), &dir.path().join("p"));
    assert!(Arc::ptr_eq(&a, &b));
}
