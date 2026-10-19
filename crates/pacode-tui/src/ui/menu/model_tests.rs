use super::*;
use crate::ui::menu::MenuId;

fn item(id: &str) -> MenuRow {
    MenuRow::Item {
        id: RowId::from(id),
        mark: Mark::None,
        label: id.to_string(),
        right: None,
        detail: Detail::default(),
    }
}

#[test]
fn sections_are_not_selectable() {
    let rows = vec![
        MenuRow::section("today"),
        item("a"),
        MenuRow::section("earlier"),
        item("b"),
        MenuRow::Submenu {
            id: RowId::from("sub"),
            label: "Sub".to_string(),
            right: None,
            target: MenuId::Settings,
        },
    ];
    let got: Vec<(usize, RowId)> = selectable(&rows).collect();
    assert_eq!(
        got,
        vec![
            (1, RowId::from("a")),
            (3, RowId::from("b")),
            (4, RowId::from("sub"))
        ]
    );
}

#[test]
fn resolve_cursor_prefers_the_remembered_id() {
    let rows = vec![MenuRow::section("s"), item("a"), item("b")];
    // An empty cursor lands on the first selectable row.
    assert_eq!(resolve_cursor(&rows, &RowId::default()), Some(1));
    // A known id wins even when it is not the first.
    assert_eq!(resolve_cursor(&rows, &RowId::from("b")), Some(2));
    // A stale id falls back to the first selectable row.
    assert_eq!(resolve_cursor(&rows, &RowId::from("gone")), Some(1));
    // No selectable rows at all.
    assert_eq!(
        resolve_cursor(&[MenuRow::section("only")], &RowId::from("x")),
        None
    );
    assert_eq!(resolve_cursor(&[], &RowId::default()), None);
}
