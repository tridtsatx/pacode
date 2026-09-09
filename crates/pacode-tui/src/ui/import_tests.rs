use std::collections::BTreeMap;
use std::path::PathBuf;

use pacode_import::{DiscoveredMcp, DiscoveredSkill, ImportSource, McpServerConfig, PlanEntry};

use super::*;

fn make_mcp(name: &str, source: ImportSource) -> PlanEntry {
    PlanEntry::mcp(DiscoveredMcp {
        source,
        origin: PathBuf::from(format!("/test/{name}.json")),
        name: name.to_string(),
        server: McpServerConfig {
            command: "echo".to_string(),
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            lazy: true,
            timeout_secs: 60,
        },
    })
}

fn make_skill(name: &str, source: ImportSource) -> PlanEntry {
    PlanEntry::skill(DiscoveredSkill {
        source,
        origin: PathBuf::from(format!("/test/skills/{name}/SKILL.md")),
        name: name.to_string(),
        description: format!("Description of {name}"),
        dir: PathBuf::from(format!("/test/skills/{name}")),
    })
}

#[test]
fn test_overlay_navigation() {
    let mut state = ImportOverlayState {
        rows: vec![
            ImportRow {
                source: ImportSource::ClaudeCode,
                entry: make_mcp("server1", ImportSource::ClaudeCode),
                checked: true,
                already_imported: false,
            },
            ImportRow {
                source: ImportSource::ClaudeCode,
                entry: make_skill("skill1", ImportSource::ClaudeCode),
                checked: true,
                already_imported: false,
            },
            ImportRow {
                source: ImportSource::Cursor,
                entry: make_mcp("server2", ImportSource::Cursor),
                checked: true,
                already_imported: false,
            },
        ],
        selected: 0,
        scroll: 0,
        closed: false,
    };

    assert_eq!(state.selected, 0);
    state.move_up(); // clamped at 0
    assert_eq!(state.selected, 0);

    state.move_down();
    assert_eq!(state.selected, 1);

    state.move_down();
    assert_eq!(state.selected, 2);

    state.move_down(); // clamped at max index (2)
    assert_eq!(state.selected, 2);

    state.move_up();
    assert_eq!(state.selected, 1);
}

#[test]
fn test_overlay_toggle_and_skip_already_imported() {
    let mut state = ImportOverlayState {
        rows: vec![
            ImportRow {
                source: ImportSource::ClaudeCode,
                entry: make_mcp("new-server", ImportSource::ClaudeCode),
                checked: true,
                already_imported: false,
            },
            ImportRow {
                source: ImportSource::ClaudeCode,
                entry: make_mcp("existing-server", ImportSource::ClaudeCode),
                checked: true,
                already_imported: true, // already imported!
            },
        ],
        selected: 0,
        scroll: 0,
        closed: false,
    };

    // Toggle row 0 (new): flips to unchecked
    state.toggle();
    assert!(!state.rows[0].checked);

    // Toggle row 0 again: flips back to checked
    state.toggle();
    assert!(state.rows[0].checked);

    // Move to row 1 (already imported)
    state.selected = 1;
    state.toggle();
    // Cannot be toggled!
    assert!(state.rows[1].checked);
    assert!(state.rows[1].already_imported);

    state.toggle();
    assert!(state.rows[1].checked);

    // checked_entries only returns non-already-imported entries
    let checked = state.checked_entries();
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].name(), "new-server");
}

#[test]
fn test_overlay_toggle_all() {
    let mut state = ImportOverlayState {
        rows: vec![
            ImportRow {
                source: ImportSource::ClaudeCode,
                entry: make_mcp("srv1", ImportSource::ClaudeCode),
                checked: true,
                already_imported: false,
            },
            ImportRow {
                source: ImportSource::ClaudeCode,
                entry: make_skill("skl1", ImportSource::ClaudeCode),
                checked: true,
                already_imported: false,
            },
            ImportRow {
                source: ImportSource::Cursor,
                entry: make_mcp("already", ImportSource::Cursor),
                checked: true,
                already_imported: true,
            },
        ],
        selected: 0,
        scroll: 0,
        closed: false,
    };

    // All normal rows are checked -> toggle_all unchecks them
    state.toggle_all();
    assert!(!state.rows[0].checked);
    assert!(!state.rows[1].checked);
    assert!(state.rows[2].checked, "already imported must stay checked");

    let checked_empty = state.checked_entries();
    assert!(checked_empty.is_empty());

    // All normal rows are unchecked -> toggle_all checks them
    state.toggle_all();
    assert!(state.rows[0].checked);
    assert!(state.rows[1].checked);
    assert!(state.rows[2].checked);

    let checked_all = state.checked_entries();
    assert_eq!(checked_all.len(), 2);
    assert_eq!(checked_all[0].name(), "srv1");
    assert_eq!(checked_all[1].name(), "skl1");

    // Partial checked state -> toggle_all checks all
    state.rows[0].checked = false;
    state.toggle_all();
    assert!(state.rows[0].checked);
    assert!(state.rows[1].checked);
    assert!(state.rows[2].checked);
}
