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

    let key = ImageCacheKey {
        path: std::path::PathBuf::from("test.png"),
        cell_w: 10,
        cell_h: 10,
    };
    files.cached_preview = Some(CachedImagePreview { key, preview: None });
    files.pending_escape = Some((
        ratatui::layout::Rect::new(0, 0, 10, 10),
        "esc".to_string(),
        std::path::PathBuf::from("test.png"),
    ));

    files.clear_preview();
    assert!(files.cached_preview.is_none());
    assert!(files.pending_escape.is_none());
}
