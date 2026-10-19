//! §5.5: the directory tree's `N · +A −R` aggregates, sections the cursor
//! cannot rest on, file names relative to their directory, the status
//! labels, `enter` → the file-diff panel, and the detail card's deferred
//! preview block.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::{CallId, Config, DiffStat};
use serde_json::json;

use super::*;
use crate::state::{Focus, Overlay};

fn state() -> AppState {
    let mut s = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    s.cwd = PathBuf::from("/repo");
    s
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// An observed `write` call plus its `DiffStat` — the `ItemAdded`/`ItemUpdated`
/// pair the event path produces.
fn write_with_diff(s: &mut AppState, path: &str, added: u32, removed: u32, call: &str, ts: u64) {
    s.files
        .observe_tool_item("write", &json!({"path": path}), ts);
    s.files.apply_diff(
        "write",
        &json!({"path": path}),
        &CallId::new(call),
        DiffStat { added, removed },
        ts,
    );
}

/// `(label, right)` of the `Section` rows in view order.
fn sections(view: &MenuView) -> Vec<(String, Option<String>)> {
    view.rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Section { label, right } => Some((label.clone(), right.clone())),
            _ => None,
        })
        .collect()
}

/// The `Item` labels in view order.
fn item_labels(view: &MenuView) -> Vec<String> {
    view.rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Item { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn dir_sections_carry_the_aggregate() {
    let mut s = state();
    write_with_diff(&mut s, "src/a.rs", 10, 2, "c1", 100);
    write_with_diff(&mut s, "src/b.rs", 3, 1, "c2", 110);
    s.files
        .observe_tool_item("read", &json!({"path": "top.rs"}), 120);

    let v = view(&s, &MenuUiState::default());
    // cwd-level files group under "." — `""` sorts before "src".
    assert_eq!(
        sections(&v),
        vec![
            (".".to_string(), Some("1".to_string())),
            ("src".to_string(), Some("2 · +13 −3".to_string())),
        ]
    );
}

#[test]
fn file_names_do_not_repeat_the_dir_prefix() {
    let mut s = state();
    write_with_diff(&mut s, "src/nested/deep/a.rs", 1, 0, "c1", 100);
    write_with_diff(&mut s, "src/nested/deep/b.rs", 0, 2, "c2", 110);

    let v = view(&s, &MenuUiState::default());
    assert_eq!(
        sections(&v),
        vec![("src/nested/deep".to_string(), Some("2 · +1 −2".to_string()))]
    );
    assert_eq!(item_labels(&v), vec!["b.rs", "a.rs"]);
    // …while the row id and the search text keep the full path.
    let ids: Vec<String> = v
        .rows
        .iter()
        .filter_map(|r| r.id().map(|id| id.as_str().to_string()))
        .collect();
    assert_eq!(ids, vec!["src/nested/deep/b.rs", "src/nested/deep/a.rs"]);
}

#[test]
fn files_and_directories_are_ordered_by_latest_touch() {
    let mut s = state();
    write_with_diff(&mut s, "old/a.rs", 1, 0, "c1", 100);
    write_with_diff(&mut s, "new/b.rs", 1, 0, "c2", 300);
    write_with_diff(&mut s, "new/c.rs", 1, 0, "c3", 200);

    let v = view(&s, &MenuUiState::default());
    assert_eq!(sections(&v)[0].0, "new");
    assert_eq!(item_labels(&v), vec!["b.rs", "c.rs", "a.rs"]);
}

#[test]
fn sections_are_not_selectable() {
    let mut s = state();
    write_with_diff(&mut s, "a.rs", 1, 0, "c1", 100);
    write_with_diff(&mut s, "src/b.rs", 1, 0, "c2", 110);
    write_with_diff(&mut s, "src/c.rs", 1, 0, "c3", 120);

    let v = view(&s, &MenuUiState::default());
    // Sections carry no id — the cursor domain excludes them by
    // construction.
    for r in &v.rows {
        assert_eq!(r.id().is_some(), !matches!(r, MenuRow::Section { .. }));
    }
    // The unset cursor lands on the first file, not the "." header.
    let sel = resolve_cursor(&v.rows, &RowId::default()).unwrap();
    assert_eq!(v.rows[sel].id().unwrap().as_str(), "src/c.rs");
}

#[test]
fn j_moves_past_sections() {
    let mut s = state();
    write_with_diff(&mut s, "a.rs", 1, 0, "c1", 100);
    write_with_diff(&mut s, "src/b.rs", 1, 0, "c2", 110);

    crate::keys_menu::open(&mut s, MenuId::Files);
    let taken = std::mem::replace(&mut s.focus, Focus::Normal);
    let Focus::Overlay(Overlay::Menu { mut stack, mut ui }) = taken else {
        panic!("menu did not open");
    };
    crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('j')));
    // `src/b.rs` → past the `src` header → `a.rs`.
    assert_eq!(ui.cursor.as_str(), "a.rs");
}

#[test]
fn status_labels_replace_numbers_where_numbers_lie() {
    let mut s = state();
    write_with_diff(&mut s, "src/gone.rs", 10, 2, "c1", 100);
    write_with_diff(&mut s, "src/new.rs", 5, 0, "c2", 110);
    write_with_diff(&mut s, "src/old.rs", 1, 1, "c3", 120);
    s.files.statuses = BTreeMap::from([
        ("src/gone.rs".to_string(), FileStatus::Deleted),
        ("src/new.rs".to_string(), FileStatus::New),
        ("src/old.rs".to_string(), FileStatus::Renamed),
    ]);

    let v = view(&s, &MenuUiState::default());
    let rights: Vec<Option<String>> = v
        .rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Item { right, .. } => Some(right.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rights,
        vec![
            Some("renamed · +1 −1".to_string()),
            Some("new · +5 −0".to_string()),
            Some("deleted".to_string())
        ]
    );
}

#[test]
fn created_heuristic_labels_new_files_before_the_scan() {
    let mut s = state();
    // An all-additions first write marks `created` — the label shows even
    // before `git status` lands.
    write_with_diff(&mut s, "src/brand.rs", 8, 0, "c1", 100);
    let v = view(&s, &MenuUiState::default());
    let right = v
        .rows
        .iter()
        .find_map(|r| match r {
            MenuRow::Item { right, .. } => right.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(right, "new · +8 −0");
}

#[test]
fn enter_opens_the_file_diff_panel() {
    let mut s = state();
    write_with_diff(&mut s, "src/a.rs", 10, 2, "c1", 100);

    crate::keys_menu::open(&mut s, MenuId::Files);
    let taken = std::mem::replace(&mut s.focus, Focus::Normal);
    let Focus::Overlay(Overlay::Menu { mut stack, mut ui }) = taken else {
        panic!("menu did not open");
    };
    let acts = crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Enter));

    assert!(acts.is_empty());
    assert!(matches!(
        s.focus,
        Focus::Panel {
            target: PanelTarget::FileDiff { ref path },
            ..
        } if path == "src/a.rs"
    ));
    assert_eq!(
        s.panel.target,
        Some(PanelTarget::FileDiff {
            path: "src/a.rs".to_string()
        })
    );
    // The probe is armed for the deferred job; no render-side git call ran.
    assert_eq!(s.files.diff_want.as_deref(), Some("src/a.rs"));
    assert!(s.panel.diff.is_none());
    assert_eq!(s.panel.scroll, usize::MAX);
}

#[test]
fn preview_want_follows_the_cursor() {
    let mut s = state();
    write_with_diff(&mut s, "a.rs", 1, 0, "c1", 100);
    write_with_diff(&mut s, "src/b.rs", 1, 0, "c2", 110);

    let mut ui = MenuUiState::default();
    let v = view(&s, &ui);
    assert_eq!(
        preview_want(&v, &ui),
        Some(PreviewKey {
            raw: "src/b.rs".to_string(),
        })
    );

    ui.cursor = RowId::new("a.rs");
    let v = view(&s, &ui);
    assert_eq!(
        preview_want(&v, &ui),
        Some(PreviewKey {
            raw: "a.rs".to_string(),
        })
    );
}

#[test]
fn detail_card_shows_the_cached_preview() {
    let mut s = state();
    write_with_diff(&mut s, "a.rs", 1, 0, "c1", 100);
    let ui = MenuUiState::default();

    // Nothing cached yet — an honest placeholder.
    let v = view(&s, &ui);
    let detail = v.rows[1].detail().unwrap();
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t == "loading preview…"))
    );

    // The job landed: the lexed head becomes `Code` lines.
    s.files.cached_preview = Some(crate::state::files::CachedPreview {
        key: PreviewKey {
            raw: "a.rs".to_string(),
        },
        resolved: PathBuf::from("/repo/a.rs"),
        text: Some(TextPreview {
            lines: vec![PreviewLine {
                text: "fn main() {}".to_string(),
                toks: vec![],
            }],
            truncated: false,
        }),
    });
    let v = view(&s, &ui);
    let detail = v.rows[1].detail().unwrap();
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Code { text, .. } if text == "fn main() {}"))
    );
}

#[test]
fn the_o_action_is_declared_and_row_chips_carry_it() {
    let mut s = state();
    write_with_diff(&mut s, "a.rs", 1, 0, "c1", 100);
    let v = view(&s, &MenuUiState::default());

    assert!(
        v.actions
            .iter()
            .any(|a| a.key.code == KeyCode::Char('o') && a.enabled)
    );
    let detail = v.rows[1].detail().unwrap();
    assert!(
        detail
            .actions
            .iter()
            .any(|a| a.key.code == KeyCode::Char('o') && a.enabled)
    );
    // `o` must not collide with a base key — the renderer would drop it.
    assert!(v.base_key_conflicts().is_empty());
}

#[test]
fn on_action_ignores_foreign_keys() {
    let mut s = state();
    write_with_diff(&mut s, "a.rs", 1, 0, "c1", 100);
    let other = MenuAction::new(
        KeyBinding {
            code: KeyCode::Char('x'),
            mods: KeyModifiers::NONE,
        },
        "noop",
    );
    let mut ui = MenuUiState::default();
    assert!(on_action(&mut s, &mut ui, &other, Some(&RowId::new("a.rs"))).is_empty());
    // Even the right key does nothing without a row.
    assert!(on_action(&mut s, &mut ui, &MenuAction::new(OPEN_KEY, "editor"), None).is_empty());
}

#[test]
fn empty_state_when_nothing_was_touched() {
    let s = state();
    let v = view(&s, &MenuUiState::default());
    assert!(v.rows.is_empty());
    assert_eq!(v.empty.primary, "no files touched yet");
    // The `o` action disables with the list.
    assert!(v.actions.iter().all(|a| !a.enabled));
}

#[test]
fn root_row_summarises_the_turn() {
    let mut s = state();
    let root = crate::ui::menu::root::view(&s, &MenuUiState::default());
    let files_row = root
        .rows
        .iter()
        .find(|r| r.id().is_some_and(|id| id.as_str() == "files"));
    assert!(files_row.is_some(), "the hub lists Files");

    write_with_diff(&mut s, "src/a.rs", 10, 2, "c1", 100);
    write_with_diff(&mut s, "src/b.rs", 0, 5, "c2", 110);
    let root = crate::ui::menu::root::view(&s, &MenuUiState::default());
    let right = root
        .rows
        .iter()
        .find_map(|r| match r {
            MenuRow::Submenu { id, right, .. } if id.as_str() == "files" => right.clone(),
            _ => None,
        })
        .unwrap();
    assert_eq!(right, "2 files · +10 −7");
}

#[test]
fn is_image_file_recognises_supported_extensions() {
    assert!(is_image_file("photo.png"));
    assert!(is_image_file("PHOTO.PNG"));
    assert!(is_image_file("image.jpg"));
    assert!(is_image_file("IMAGE.JPEG"));
    assert!(is_image_file("animation.gif"));
    assert!(is_image_file("pic.webp"));
    assert!(is_image_file("bitmap.bmp"));

    assert!(!is_image_file("code.rs"));
    assert!(!is_image_file("document.pdf"));
    assert!(!is_image_file("archive.tar.gz"));
    assert!(!is_image_file("no_extension"));
    assert!(!is_image_file(""));
}

#[test]
fn text_preview_reads_and_lexes_the_head() {
    let dir = std::env::temp_dir().join(format!("pacode_preview_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.rs");
    std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();

    let t = text_preview(&file).unwrap();
    assert_eq!(t.lines.len(), 3);
    assert!(!t.truncated);
    // A Rust file lexes — the head carries tokens, not bare text.
    assert!(t.lines.iter().any(|l| !l.toks.is_empty()));

    // A missing file is `None`, not a panic.
    assert!(text_preview(&dir.join("ghost.rs")).is_none());
    // An image extension never opens the file at all.
    std::fs::write(dir.join("pic.png"), b"not really a png").unwrap();
    assert!(text_preview(&dir.join("pic.png")).is_none());
    // A NUL marks a binary.
    std::fs::write(dir.join("bin.dat"), b"a\0b").unwrap();
    assert!(text_preview(&dir.join("bin.dat")).is_none());

    std::fs::remove_dir_all(&dir).ok();
}
