use pretty_assertions::assert_eq;

use crate::{OldLines, added_by_set, added_line_indices};

#[test]
fn inserted_lines_are_reported() {
    let idx = added_line_indices(Some("a\nb\nc\n"), "a\nX\nb\nc\nY\n");
    assert_eq!(idx, vec![1, 4]);
}

#[test]
fn changed_line_counts_as_added() {
    assert_eq!(added_line_indices(Some("a\nb\nc\n"), "a\nB\nc\n"), vec![1]);
}

#[test]
fn unchanged_and_deleted_yield_nothing() {
    assert!(added_line_indices(Some("a\nb\n"), "a\nb\n").is_empty());
    assert!(added_line_indices(Some("a\nb\n"), "a\n").is_empty());
}

#[test]
fn new_file_is_all_added() {
    assert_eq!(added_line_indices(None, "a\nb\nc"), vec![0, 1, 2]);
}

#[test]
fn set_based_ignores_blank_and_indentation() {
    let old = OldLines::new("  keep\n");
    assert_eq!(added_by_set(&old, "keep\n\nnew\n    keep\n"), vec![2]);
}

#[test]
fn pathological_full_rewrite_is_fast_and_marks_all_added() {
    let old: String = (0..20_000).map(|i| format!("old line {i}\n")).collect();
    let new: String = (0..20_000).map(|i| format!("new line {i}\n")).collect();
    let start = std::time::Instant::now();
    let idx = added_line_indices(Some(&old), &new);
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(idx.len(), 20_000);
}
