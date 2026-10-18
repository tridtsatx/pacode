use super::*;

#[test]
fn hash_is_two_symbols_and_stable() {
    let h = line_hash("fn main() {}");
    assert_eq!(h.len(), HASH_LEN);
    assert_eq!(h, line_hash("fn main() {}"));
    assert!(h.chars().all(|c| c.is_ascii_alphanumeric()));
}

#[test]
fn hash_ignores_trailing_whitespace_but_not_indent() {
    assert_eq!(line_hash("let x = 1;"), line_hash("let x = 1;  \r"));
    assert_ne!(line_hash("let x = 1;"), line_hash("    let x = 1;"));
}

#[test]
fn hash_spreads_over_many_values() {
    let distinct: std::collections::HashSet<String> = (0..500)
        .map(|i| line_hash(&format!("line number {i}")))
        .collect();
    assert!(
        distinct.len() > 250,
        "only {} distinct hashes",
        distinct.len()
    );
}

#[test]
fn anchor_parse_forms() {
    let h = line_hash("x");
    assert_eq!(
        Anchor::parse(&format!("12#{h}")).unwrap(),
        Anchor::Line {
            line: 12,
            hash: h.clone()
        }
    );
    assert_eq!(
        Anchor::parse(&format!("12#{h}| x")).unwrap(),
        Anchor::Line {
            line: 12,
            hash: h.clone()
        }
    );
    assert_eq!(
        Anchor::parse(&format!(" 7#{} ", h.to_uppercase())).unwrap(),
        Anchor::Line { line: 7, hash: h }
    );
    assert_eq!(Anchor::parse("bof").unwrap(), Anchor::Bof);
    assert_eq!(Anchor::parse("EOF").unwrap(), Anchor::Eof);
}

#[test]
fn anchor_parse_rejects_garbage() {
    for bad in [
        "", "12", "#ab", "0#ab", "x#ab", "12#a", "12#abc", "12#!!", "-3#ab",
    ] {
        assert!(
            matches!(Anchor::parse(bad), Err(EditError::InvalidAnchor(_))),
            "{bad:?} should be invalid"
        );
    }
}

#[test]
fn anchor_display_round_trips() {
    let a = Anchor::for_line(42, "hello");
    assert_eq!(Anchor::parse(&a.to_string()).unwrap(), a);
}

#[test]
fn op_deserializes_from_json() {
    let h = line_hash("a");
    let op: Op = serde_json::from_value(serde_json::json!({
        "op": "replace", "start": format!("1#{h}"), "lines": ["b"]
    }))
    .unwrap();
    assert_eq!(
        op,
        Op::Replace {
            start: Anchor::Line { line: 1, hash: h },
            end: None,
            lines: vec!["b".to_string()]
        }
    );
    let bad = serde_json::from_value::<Op>(serde_json::json!({
        "op": "delete", "start": "nonsense"
    }));
    assert!(bad.is_err());
}

#[test]
fn annotate_numbers_from_start_line() {
    let out = annotate("a\n\nb\n", 10);
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], format!("10#{}| a", line_hash("a")));
    assert_eq!(rows[1], format!("11#{}| ", line_hash("")));
    assert_eq!(rows[2], format!("12#{}| b", line_hash("b")));
}

#[test]
fn annotate_handles_crlf_unicode_and_empty() {
    let out = annotate("héllo wörld ✓\r\nnext\r\n", 1);
    assert!(out.starts_with(&format!("1#{}| héllo wörld ✓", line_hash("héllo wörld ✓"))));
    assert!(!out.contains('\r'));
    assert_eq!(annotate("", 1), "");
}

#[test]
fn annotate_truncates_long_lines_but_hashes_full_line() {
    let long = "é".repeat(MAX_DISPLAY_CHARS + 50);
    let out = annotate(&long, 1);
    assert!(out.contains("...[50 more chars]"));
    assert!(out.starts_with(&format!("1#{}| ", line_hash(&long))));
}

#[test]
#[allow(clippy::single_range_in_vec_init)]
fn snippet_merges_windows_and_marks_focus() {
    let content: String = (1..=30).map(|i| format!("l{i}\n")).collect();
    let out = snippet(&content, &[9..10, 11..12], 2, 100, true);
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), 7, "{out}");
    assert!(rows.iter().filter(|r| r.starts_with('>')).count() == 2);
    let far = snippet(&content, &[0..1, 25..26], 1, 100, false);
    assert!(far.contains("\n...\n"));
    let capped = snippet(&content, &[0..30], 0, 5, false);
    assert!(capped.ends_with("(25 more lines not shown)"));
    assert_eq!(snippet("", &[0..1], 3, 10, false), "(file is empty)");
}
