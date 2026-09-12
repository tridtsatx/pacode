use pretty_assertions::assert_eq;

use super::*;

const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";
const H3: &str = "3333333333333333333333333333333333333333";

fn rec(fields: &[&str]) -> String {
    format!("{}\u{1e}", fields.join("\u{1f}"))
}

#[test]
fn parses_single_commit() {
    let out = rec(&[H1, "1111111", "", "", "init", "Ada", "1700000000", ""]);
    let rows = parse_log(out.as_bytes());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0],
        CommitRow {
            hash: H1.into(),
            short: "1111111".into(),
            parents: vec![],
            refs: vec![],
            subject: "init".into(),
            body: String::new(),
            author: "Ada".into(),
            date_ms: 1_700_000_000_000,
        }
    );
}

#[test]
fn parses_parents_and_multiline_body() {
    let out = rec(&[
        H1,
        "1111111",
        &format!("{H2} {H3}"),
        "",
        "merge",
        "Bob",
        "1700000001",
        "line one\nline two\n",
    ]);
    let rows = parse_log(out.as_bytes());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].parents, vec![H2.to_string(), H3.to_string()]);
    assert_eq!(rows[0].body, "line one\nline two");
}

#[test]
fn decorations_full_format() {
    let out = rec(&[
        H1,
        "1111111",
        "",
        "HEAD -> refs/heads/main, tag: refs/tags/v1.0, refs/remotes/origin/main",
        "s",
        "A",
        "1700000000",
        "",
    ]);
    let rows = parse_log(out.as_bytes());
    assert_eq!(
        rows[0].refs,
        vec![
            RefBadge::Head,
            RefBadge::Branch("main".into()),
            RefBadge::Tag("v1.0".into()),
            RefBadge::Remote("origin/main".into()),
        ]
    );
}

#[test]
fn decorations_detached_and_short_names() {
    let out = rec(&[
        H1,
        "1111111",
        "",
        "HEAD, v2, origin/dev, tag: v2",
        "s",
        "A",
        "1700000000",
        "",
    ]);
    let rows = parse_log(out.as_bytes());
    assert_eq!(
        rows[0].refs,
        vec![
            RefBadge::Head,
            RefBadge::Branch("v2".into()),
            RefBadge::Branch("origin/dev".into()),
            RefBadge::Tag("v2".into()),
        ]
    );
}

#[test]
fn skips_malformed_records_and_empty_tail() {
    let good = rec(&[H1, "1111111", "", "", "ok", "A", "1700000000", ""]);
    let out = format!("{good}garbage-no-seps\n{good}");
    let rows = parse_log(out.as_bytes());
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.subject == "ok"));
}

#[test]
fn field_sep_inside_body_is_kept() {
    let out = rec(&[H1, "1111111", "", "", "s", "A", "1700000000", "a\u{1f}b"]);
    let rows = parse_log(out.as_bytes());
    assert_eq!(rows[0].body, "a\u{1f}b");
}

#[test]
fn empty_output_is_empty() {
    assert!(parse_log(b"").is_empty());
    assert!(parse_log(b"\n").is_empty());
}
