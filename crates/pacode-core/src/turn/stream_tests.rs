use super::{RAW_ARGS_CAP, finish_args};

#[test]
fn complete_args_parse_without_error() {
    let (err, val) = finish_args(r#"{"path":"a.rs","limit":10}"#, false);
    assert!(err.is_none());
    assert_eq!(val["path"], "a.rs");
    assert_eq!(val["limit"], 10);
}

#[test]
fn complete_but_invalid_args_report_a_real_parse_error() {
    let (err, val) = finish_args(r#"{"path": }"#, false);
    let err = err.expect("invalid complete args must report a parse error");
    // A genuine syntax error, not the cap message.
    assert!(
        !err.contains("streaming cap"),
        "expected a syntax error, got: {err}"
    );
    assert_eq!(val, serde_json::json!({}));
}

#[test]
fn capped_args_report_the_cap_not_a_syntax_error() {
    // Exactly what the buffer holds after the cut: a JSON prefix.
    let prefix = format!(r#"{{"path":"{}"#, "x".repeat(1024));
    let (err, val) = finish_args(&prefix, true);
    let err = err.expect("capped args must not be dispatched");
    assert!(
        err.contains("streaming cap"),
        "expected the cap diagnostic, got: {err}"
    );
    assert!(
        !err.contains("expected"),
        "cap diagnostic must not leak a JSON syntax complaint: {err}"
    );
    assert_eq!(val, serde_json::json!({}));
}

#[test]
fn capped_args_of_exactly_the_cap_size_are_still_refused() {
    // The cap is on accumulated length, not on parseability. Even a buffer
    // that happens to hold a whole-but-oversized object is refused, because we
    // cannot know what the tail after the cut was.
    let big = format!(r#"{{"data":"{}"}}"#, "y".repeat(RAW_ARGS_CAP));
    assert!(big.len() > RAW_ARGS_CAP);
    let (err, val) = finish_args(&big, true);
    assert!(
        err.expect("oversized args must not be dispatched")
            .contains("streaming cap")
    );
    assert_eq!(val, serde_json::json!({}));
}
