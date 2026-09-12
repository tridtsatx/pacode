use super::*;
use serde_json::json;

#[test]
fn test_observe_tool_item_kinds_and_badges() {
    let mut files = FilesState::new();

    // Read tool
    files.observe_tool_item("read_file", &json!({"path": "src/main.rs"}), 100);
    assert_eq!(files.len(), 1);
    let row = &files.rows[0];
    assert_eq!(row.path, "src/main.rs");
    assert!(row.kinds.has_read());
    assert!(!row.kinds.has_write());
    assert_eq!(row.count, 1);
    assert_eq!(row.last_ts_ms, 100);

    // Write tool on another file
    files.observe_tool_item("write", &json!({"file_path": "src/out.txt"}), 200);
    assert_eq!(files.len(), 2);
    let row_out = files.rows.iter().find(|r| r.path == "src/out.txt").unwrap();
    assert!(row_out.kinds.has_write());
    assert!(!row_out.kinds.has_read());

    // Edit tool on first file (accumulates kinds & count)
    files.observe_tool_item("edit_file", &json!({"path": "src/main.rs"}), 300);
    assert_eq!(files.len(), 2);
    let row_main = files.rows.iter().find(|r| r.path == "src/main.rs").unwrap();
    assert!(row_main.kinds.has_read());
    assert!(row_main.kinds.has_edit());
    assert!(!row_main.kinds.has_write());
    assert_eq!(row_main.count, 2);
    assert_eq!(row_main.last_ts_ms, 300);

    // Search tool (grep) with dir
    files.observe_tool_item("grep", &json!({"dir": "crates/"}), 400);
    let row_dir = files.rows.iter().find(|r| r.path == "crates/").unwrap();
    assert!(row_dir.kinds.has_search());
}

#[test]
fn test_observe_paths_array() {
    let mut files = FilesState::new();
    files.observe_tool_item("glob", &json!({"paths": ["file_a.rs", "file_b.rs"]}), 500);
    assert_eq!(files.len(), 2);
    assert!(files.rows.iter().any(|r| r.path == "file_a.rs"));
    assert!(files.rows.iter().any(|r| r.path == "file_b.rs"));
}

#[test]
fn test_ignore_mcp_and_unknown_tools() {
    let mut files = FilesState::new();
    // MCP tool ignored
    files.observe_tool_item("server__read", &json!({"path": "file.rs"}), 100);
    assert_eq!(files.len(), 0);

    // Unknown tool ignored
    files.observe_tool_item("bash", &json!({"command": "cargo build"}), 200);
    assert_eq!(files.len(), 0);
}

#[test]
fn test_eviction_cap_at_500() {
    let mut files = FilesState::new();
    for i in 0..550 {
        let path = format!("file_{i}.rs");
        files.observe_tool_item("read", &json!({"path": path}), i as u64);
    }
    assert_eq!(files.len(), MAX_FILES);
    // Oldest 50 files (ts 0..50) should have been evicted
    assert!(!files.rows.iter().any(|r| r.path == "file_0.rs"));
    assert!(!files.rows.iter().any(|r| r.path == "file_49.rs"));
    assert!(files.rows.iter().any(|r| r.path == "file_50.rs"));
    assert!(files.rows.iter().any(|r| r.path == "file_549.rs"));
}

#[test]
fn test_sorted_rows_desc() {
    let mut files = FilesState::new();
    files.observe_tool_item("read", &json!({"path": "old.rs"}), 100);
    files.observe_tool_item("read", &json!({"path": "newest.rs"}), 300);
    files.observe_tool_item("read", &json!({"path": "mid.rs"}), 200);

    let sorted = files.sorted_rows();
    assert_eq!(sorted.len(), 3);
    assert_eq!(sorted[0].path, "newest.rs");
    assert_eq!(sorted[1].path, "mid.rs");
    assert_eq!(sorted[2].path, "old.rs");
}

#[test]
fn test_clear_preview() {
    let mut files = FilesState::new();
    assert!(files.cached_preview.is_none());
    assert!(files.pending_escape.is_none());

    let key = PreviewKey {
        raw: "test.png".to_string(),
        kind: PreviewKind::Image {
            cell_w: 10,
            cell_h: 10,
        },
    };
    files.cached_preview = Some(CachedPreview {
        key,
        resolved: std::path::PathBuf::from("test.png"),
        preview: None,
        text: None,
    });
    files.pending_escape = Some((
        ratatui::layout::Rect::new(0, 0, 10, 10),
        "esc".to_string(),
        std::path::PathBuf::from("test.png"),
    ));
    files.preview_want = Some(PreviewKey {
        raw: "other.png".to_string(),
        kind: PreviewKind::Image {
            cell_w: 10,
            cell_h: 10,
        },
    });
    files.preview_in_flight = files.preview_want.clone();

    files.clear_preview();
    assert!(files.cached_preview.is_none());
    assert!(files.pending_escape.is_none());
    assert!(files.preview_want.is_none());
    assert!(files.preview_in_flight.is_none());
}

#[test]
fn test_apply_diff_folds_stats_once_per_call() {
    let mut files = FilesState::new();
    files.observe_tool_item("write", &json!({"path": "src/new.rs"}), 100);
    let call = CallId::new("call_1");

    files.apply_diff(
        "write",
        &json!({"path": "src/new.rs"}),
        &call,
        DiffStat {
            added: 12,
            removed: 0,
        },
        150,
    );
    let row = &files.rows[0];
    assert_eq!((row.added, row.removed), (12, 0));
    assert!(row.has_diff);
    assert!(row.created, "an all-add first diff means the file is new");

    // The same completed item arriving again must not double-count.
    files.apply_diff(
        "write",
        &json!({"path": "src/new.rs"}),
        &call,
        DiffStat {
            added: 12,
            removed: 0,
        },
        160,
    );
    assert_eq!(files.rows[0].added, 12);
}

#[test]
fn test_apply_diff_accumulates_and_marks_existing() {
    let mut files = FilesState::new();
    files.observe_tool_item("edit", &json!({"path": "src/main.rs"}), 100);
    files.apply_diff(
        "edit",
        &json!({"path": "src/main.rs"}),
        &CallId::new("call_1"),
        DiffStat {
            added: 4,
            removed: 2,
        },
        150,
    );
    files.apply_diff(
        "edit",
        &json!({"path": "src/main.rs"}),
        &CallId::new("call_2"),
        DiffStat {
            added: 1,
            removed: 3,
        },
        200,
    );
    let row = &files.rows[0];
    assert_eq!((row.added, row.removed), (5, 5));
    assert!(row.has_diff);
    assert!(!row.created);
    assert_eq!(row.last_ts_ms, 200);
}

#[test]
fn test_apply_diff_ignores_reads_and_missing_rows() {
    let mut files = FilesState::new();
    files.observe_tool_item("read", &json!({"path": "src/main.rs"}), 100);
    files.apply_diff(
        "read",
        &json!({"path": "src/main.rs"}),
        &CallId::new("call_1"),
        DiffStat {
            added: 1,
            removed: 0,
        },
        150,
    );
    assert!(!files.rows[0].has_diff, "a read call carries no diff");

    // A diff for a row the ItemAdded path never made is dropped.
    files.apply_diff(
        "edit",
        &json!({"path": "ghost.rs"}),
        &CallId::new("call_2"),
        DiffStat {
            added: 3,
            removed: 1,
        },
        160,
    );
    assert_eq!(files.len(), 1);
}

#[test]
fn test_status_of_looks_up_the_row_spelling() {
    // `statuses` is keyed by the exact string the scan was asked about (the
    // row's own path) — `worktree_statuses` already mapped git's
    // root-relative output back to it, so the lookup is literal.
    let mut files = FilesState::new();
    files
        .statuses
        .insert("./src/main.rs".to_string(), FileStatus::Modified);
    assert_eq!(files.status_of("./src/main.rs"), Some(FileStatus::Modified));
    // No normalization at read time: a different spelling is a different row.
    assert_eq!(files.status_of("src/main.rs"), None);
}

#[test]
fn test_rows_version_tracks_staleness() {
    let mut files = FilesState::new();
    assert_eq!(files.statused_at, None);
    let v0 = files.rows_version;
    files.observe_tool_item("read", &json!({"path": "a.rs"}), 100);
    assert!(files.rows_version > v0);
    let v1 = files.rows_version;
    files.apply_statuses(BTreeMap::new(), v1);
    assert_eq!(files.statused_at, Some(v1));
    assert!(!files.statuses_in_flight);
}

#[test]
fn test_title_path_arg() {
    assert_eq!(title_path_arg("write", "Write src/x.rs"), "src/x.rs");
    assert_eq!(title_path_arg("edit", "Edit a/b.rs"), "a/b.rs");
    assert_eq!(title_path_arg("read", "Read top.rs"), "top.rs");
    // A title that is just the verb stays whole.
    assert_eq!(title_path_arg("write", "Write"), "Write");
}
