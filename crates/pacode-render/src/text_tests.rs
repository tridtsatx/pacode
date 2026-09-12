use super::*;
use crate::RenderOptions;

#[test]
fn plural_picks_one_few_many() {
    let lines = |n| plural(n, "строка", "строки", "строк");
    assert_eq!(lines(1), "строка");
    assert_eq!(lines(2), "строки");
    assert_eq!(lines(4), "строки");
    assert_eq!(lines(5), "строк");
    assert_eq!(lines(11), "строк");
    assert_eq!(lines(14), "строк");
    assert_eq!(lines(21), "строка");
    assert_eq!(lines(22), "строки");
    assert_eq!(lines(25), "строк");
    assert_eq!(lines(111), "строк");
    assert_eq!(lines(101), "строка");
}

#[test]
fn more_lines_marks_the_fold() {
    let opts = RenderOptions::new(80, false);
    assert_eq!(more_lines(12, &opts), "… ещё 12 строк · ctrl+o развернуть");
    assert_eq!(more_lines(1, &opts), "… ещё 1 строка · ctrl+o развернуть");
    let ascii = RenderOptions::new(80, true);
    assert_eq!(
        more_lines(3, &ascii),
        "... ещё 3 строки . ctrl+o развернуть"
    );
}

#[test]
fn tool_label_covers_the_named_tools_and_leaves_the_rest() {
    assert_eq!(tool_label("bash"), Some("Команда"));
    assert_eq!(tool_label("read"), Some("Чтение"));
    assert_eq!(tool_label("grep"), Some("Поиск"));
    assert_eq!(tool_label("write"), Some("Запись"));
    assert_eq!(tool_label("edit"), Some("Правка"));
    assert_eq!(tool_label("github__create_issue"), None);
}
