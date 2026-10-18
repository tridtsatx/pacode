use std::path::Path;

use pretty_assertions::assert_eq;

use crate::{
    LoadOptions, MAX_FINDINGS_PER_RULE, ModeFilter, OldLines, RuleSet, Severity, scan_added,
    scan_added_fast,
};

fn set() -> RuleSet {
    RuleSet::load(&LoadOptions {
        builtin: true,
        ..LoadOptions::default()
    })
    .set
}

#[test]
fn only_added_lines_are_scanned() {
    let old = "const a: any = 1;\nconst b = 2;\n";
    let new = "const a: any = 1;\nconst b = 2;\nconst c: any = 3;\n";
    let report = scan_added(
        &set(),
        Path::new("a.ts"),
        Some(old),
        new,
        ModeFilter::AfterWrite,
    );
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].line, 3);
    assert_eq!(report.findings[0].snippet, "const c: any = 3;");
    assert_eq!(report.findings[0].severity, Severity::Block);
    assert!(report.has_block());
}

#[test]
fn rewriting_a_file_with_old_violations_stays_quiet() {
    let old = "var a = 1;\nconsole.log(a);\n";
    let new = "var a = 1;\nconsole.log(a);\nconst z = 3;\n";
    let report = scan_added(
        &set(),
        Path::new("a.ts"),
        Some(old),
        new,
        ModeFilter::AfterWrite,
    );
    assert!(report.is_empty(), "{:?}", report.findings);
}

#[test]
fn new_file_scans_every_line() {
    let new = "var a = 1;\nconsole.log(a);\n";
    let report = scan_added(&set(), Path::new("a.ts"), None, new, ModeFilter::AfterWrite);
    assert_eq!(report.rule_names(), vec!["ts-no-var", "ts-no-console-log"]);
}

#[test]
fn stream_filter_only_runs_stream_rules() {
    let new = "var a = 1;\nconst x: any = 1;\n";
    let report = scan_added(&set(), Path::new("a.ts"), None, new, ModeFilter::StreamOnly);
    assert_eq!(report.rule_names(), vec!["ts-no-explicit-any"]);
    let rust = scan_added(
        &set(),
        Path::new("a.rs"),
        None,
        "let a = x.unwrap();\ndbg!(a);\n",
        ModeFilter::StreamOnly,
    );
    assert_eq!(rust.rule_names(), vec!["rust-no-dbg"]);
}

#[test]
fn unknown_extension_has_no_rules() {
    let report = scan_added(
        &set(),
        Path::new("a.py"),
        None,
        "x: any\n",
        ModeFilter::AfterWrite,
    );
    assert!(report.is_empty());
}

#[test]
fn per_rule_cap_counts_the_rest_as_suppressed() {
    let new: String = (0..8)
        .map(|i| format!("const v{i}: any = {i};\n"))
        .collect();
    let report = scan_added(
        &set(),
        Path::new("a.ts"),
        None,
        &new,
        ModeFilter::AfterWrite,
    );
    assert_eq!(report.findings.len(), MAX_FINDINGS_PER_RULE);
    assert_eq!(report.suppressed, 3);
}

#[test]
fn fast_scan_uses_line_set_of_old_text() {
    let old = OldLines::new("const a: any = 1;\n");
    let new = "const a: any = 1;\n  const b: any = 2;\n";
    let report = scan_added_fast(&set(), Path::new("a.ts"), &old, new, ModeFilter::StreamOnly);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].line, 2);
}

#[test]
fn multibyte_text_does_not_panic() {
    let new = "const s = \"héllo → wörld\"; const x: any = 1; // ünïcode ≠ ascii\nif (é == ü) {}\n";
    let report = scan_added(&set(), Path::new("a.ts"), None, new, ModeFilter::AfterWrite);
    assert!(
        report
            .rule_names()
            .contains(&"ts-no-explicit-any".to_string())
    );
    assert!(
        report
            .rule_names()
            .contains(&"ts-strict-equality".to_string())
    );
}

#[test]
fn block_comment_spanning_lines_is_masked() {
    let new = "/*\n const x: any = 1;\n*/\nconst ok = 1;\n";
    let report = scan_added(&set(), Path::new("a.ts"), None, new, ModeFilter::AfterWrite);
    assert!(report.is_empty(), "{:?}", report.findings);
}
