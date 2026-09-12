use super::*;
use crate::RenderOptions;

#[test]
fn lines_noun_singular_and_plural() {
    assert_eq!(lines_noun(1), "line");
    assert_eq!(lines_noun(2), "lines");
    assert_eq!(lines_noun(0), "lines");
    assert_eq!(lines_noun(111), "lines");
}

#[test]
fn more_lines_marks_the_fold() {
    let opts = RenderOptions::new(80, false);
    assert_eq!(more_lines(12, &opts), "… 12 more lines · ctrl+o to expand");
    assert_eq!(more_lines(1, &opts), "… 1 more line · ctrl+o to expand");
    let ascii = RenderOptions::new(80, true);
    assert_eq!(more_lines(3, &ascii), "... 3 more lines . ctrl+o to expand");
}

#[test]
fn more_lines_uses_the_bound_key() {
    let opts = RenderOptions::new(80, false).expand_hint("f9");
    assert_eq!(more_lines(4, &opts), "… 4 more lines · f9 to expand");
}

#[test]
fn tool_label_covers_the_named_tools_and_leaves_the_rest() {
    assert_eq!(tool_label("bash"), Some("Command"));
    assert_eq!(tool_label("read"), Some("Read"));
    assert_eq!(tool_label("grep"), Some("Search"));
    assert_eq!(tool_label("write"), Some("Write"));
    assert_eq!(tool_label("edit"), Some("Edit"));
    assert_eq!(tool_label("github__create_issue"), None);
}
