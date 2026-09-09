use std::fs;
use tempfile::tempdir;

use super::*;

#[test]
fn test_complete_at_path_respects_gitignore() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // Create files
    fs::write(root.join("visible.rs"), "pub fn foo() {}").unwrap();
    fs::write(root.join("ignored.log"), "secret log").unwrap();
    fs::write(root.join(".gitignore"), "*.log\n").unwrap();

    let candidates = complete_at_path("", root);
    let paths: Vec<&str> = candidates.iter().map(|c| c.path.as_str()).collect();

    assert!(paths.contains(&"visible.rs"));
    assert!(!paths.contains(&"ignored.log"));
    assert!(!paths.contains(&".git/"));
}

#[test]
fn test_complete_at_path_directories_suffix() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("README.md"), "# Hello").unwrap();

    let candidates = complete_at_path("", root);
    let src_cand = candidates.iter().find(|c| c.path == "src/").unwrap();
    assert!(src_cand.is_dir);

    // Subdirectory navigation
    let sub_candidates = complete_at_path("src/", root);
    assert_eq!(sub_candidates.len(), 1);
    assert_eq!(sub_candidates[0].path, "src/main.rs");
    assert!(!sub_candidates[0].is_dir);
}

#[test]
fn test_complete_at_path_capped() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    for i in 0..50 {
        fs::write(root.join(format!("file_{i:02}.txt")), "").unwrap();
    }

    let candidates = complete_at_path("file_", root);
    assert_eq!(candidates.len(), MAX_AT_CANDIDATES);
}

#[test]
fn test_accepting_directory_keeps_popup_open() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("subdir")).unwrap();
    fs::write(root.join("subdir").join("nested.txt"), "hello").unwrap();

    let mut state =
        crate::state::AppState::new(pacode_types::Config::default(), "0.1.0".to_string(), 80, 24);
    state.cwd = root.to_path_buf();
    state.input.text = "read @sub".to_string();
    state.input.cursor = 9;

    // Press Tab to accept candidate 'subdir/'
    let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    let actions = crate::keys::handle_key(&mut state, key, std::time::Instant::now());
    assert!(actions.is_empty());

    // Text is now 'read @subdir/'
    assert_eq!(state.input.text, "read @subdir/");
    // Popup is NOT closed
    assert!(!state.input.at_closed);

    // Active query is now 'subdir/'
    let byte_cursor =
        crate::state::input::char_to_byte_index(&state.input.text, state.input.cursor);
    let q = pacode_types::at_ref::find_active_query(&state.input.text, byte_cursor).unwrap();
    assert_eq!(q.query, "subdir/");

    // Candidates in subdir are available
    let nested_candidates = complete_at_path(&q.query, &state.cwd());
    assert_eq!(nested_candidates.len(), 1);
    assert_eq!(nested_candidates[0].path, "subdir/nested.txt");
}
