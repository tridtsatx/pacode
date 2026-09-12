use super::*;

#[test]
fn a_direct_entry_is_a_single_level() {
    let stack = MenuStack::root(MenuId::Plugins);
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Plugins));
    // esc on a single level empties the stack — the caller closes the menu.
    let mut stack = stack;
    assert_eq!(stack.pop().map(|l| l.id), Some(MenuId::Plugins));
    assert!(stack.is_empty());
    assert_eq!(stack.pop(), None);
}

#[test]
fn push_and_pop_walk_the_levels() {
    let mut stack = MenuStack::root(MenuId::Root);
    assert!(stack.push(MenuLevel::new(MenuId::Plugins)));
    assert!(stack.push(MenuLevel::new(MenuId::Keys)));
    assert_eq!(stack.depth(), 3);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Keys));
    assert_eq!(stack.pop().map(|l| l.id), Some(MenuId::Keys));
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Plugins));
    assert_eq!(stack.levels()[0].id, MenuId::Root);
}

#[test]
#[should_panic(expected = "deeper than 4 levels")]
fn a_fifth_level_is_rejected() {
    let mut stack = MenuStack::root(MenuId::Root);
    for id in [MenuId::Settings, MenuId::Git, MenuId::Help] {
        assert!(stack.push(MenuLevel::new(id)));
    }
    assert_eq!(stack.depth(), MAX_DEPTH);
    // The fifth push is a broken hierarchy: debug_assert fires.
    let _ = stack.push(MenuLevel::new(MenuId::Import));
}

#[test]
fn a_level_saves_and_restores_its_ui_state() {
    let mut stack = MenuStack::root(MenuId::Sessions);
    let mut ui = MenuUiState {
        cursor: RowId::from("session-7"),
        scroll: 3,
        query: "fix".to_string(),
        tab: 1,
        mode: MenuMode::List,
        remote_at: None,
        status: None,
    };
    stack.current_mut().unwrap().save(&ui);

    // Descend, then come back: the level returns its snapshot in List mode.
    assert!(stack.push(MenuLevel::new(MenuId::Git)));
    stack.pop();
    ui.mode = MenuMode::Edit {
        buffer: "junk".to_string(),
        cursor: 1,
    };
    let restored = stack.current().unwrap().ui();
    assert_eq!(restored.cursor, RowId::from("session-7"));
    assert_eq!(restored.scroll, 3);
    assert_eq!(restored.query, "fix");
    assert_eq!(restored.tab, 1);
    assert_eq!(
        restored.mode,
        MenuMode::List,
        "an edit never survives a descent"
    );
}
