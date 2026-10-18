use pretty_assertions::assert_eq;

use crate::extract_string_fields;

fn f(key: &str, value: &str, complete: bool) -> crate::StrField {
    crate::StrField {
        key: key.to_string(),
        value: value.to_string(),
        complete,
    }
}

#[test]
fn complete_and_partial_values() {
    let json = r#"{"path":"src/a.ts","content":"const x: any = 1;\nlet y"#;
    let got = extract_string_fields(json, &["path", "content"]);
    assert_eq!(
        got,
        vec![
            f("path", "src/a.ts", true),
            f("content", "const x: any = 1;\nlet y", false)
        ]
    );
}

#[test]
fn escapes_are_decoded() {
    let json = r#"{"content":"a\"b\\c\tdé 😀 \/"}"#;
    let got = extract_string_fields(json, &["content"]);
    assert_eq!(got, vec![f("content", "a\"b\\c\tdé 😀 /", true)]);
}

#[test]
fn truncated_escape_is_incomplete() {
    let got = extract_string_fields(r#"{"content":"abc\"#, &["content"]);
    assert_eq!(got, vec![f("content", "abc", false)]);
    let got = extract_string_fields(r#"{"content":"abc\u00"#, &["content"]);
    assert_eq!(got, vec![f("content", "abc", false)]);
}

#[test]
fn nested_multi_edit_fields_in_order() {
    let json = r#"{"path":"a.ts","edits":[{"old_string":"x","new_string":"1"},{"old_string":"y","new_string":"2"}]}"#;
    let got = extract_string_fields(json, &["new_string"]);
    assert_eq!(
        got,
        vec![f("new_string", "1", true), f("new_string", "2", true)]
    );
}

#[test]
fn string_values_that_look_like_keys_are_not_confused() {
    let json = r#"{"content":"\"path\": \"evil\"","path":"a.ts"}"#;
    let got = extract_string_fields(json, &["path"]);
    assert_eq!(got, vec![f("path", "a.ts", true)]);
}

#[test]
fn non_string_values_clear_the_pending_key() {
    let json = r#"{"content":12,"x":"v"}"#;
    assert!(extract_string_fields(json, &["content"]).is_empty());
}

#[test]
fn empty_and_garbage_do_not_panic() {
    assert!(extract_string_fields("", &["a"]).is_empty());
    assert!(extract_string_fields("\"", &["a"]).is_empty());
    assert!(extract_string_fields("{{{\"a", &["a"]).is_empty());
}
