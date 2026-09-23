use super::*;

#[test]
fn parses_level_and_explanation() {
    let (level, text) = parse_output("LEVEL: high\nEXPLANATION: It deletes files recursively.");
    assert_eq!(level, Some(ExplainLevel::High));
    assert_eq!(text, "It deletes files recursively.");
}

#[test]
fn parses_every_level_variant() {
    for (raw, expected) in [
        ("none", ExplainLevel::None),
        ("low", ExplainLevel::Low),
        ("medium", ExplainLevel::Medium),
        ("high", ExplainLevel::High),
        ("catastrophic", ExplainLevel::Catastrophic),
    ] {
        let (level, text) = parse_output(&format!("LEVEL: {raw}\nEXPLANATION: ok"));
        assert_eq!(level, Some(expected));
        assert_eq!(text, "ok");
    }
}

#[test]
fn missing_level_still_emits_text() {
    let (level, text) = parse_output("EXPLANATION: Reads a file from disk.");
    assert_eq!(level, None);
    assert_eq!(text, "Reads a file from disk.");
}

#[test]
fn unparseable_level_degrades_to_none() {
    let (level, text) = parse_output("LEVEL: spicy\nEXPLANATION: Runs a command.");
    assert_eq!(level, None);
    assert_eq!(text, "Runs a command.");
}

#[test]
fn tags_match_case_insensitively_after_whitespace() {
    let (level, text) = parse_output("  level: Low\n  Explanation: Lists a directory.");
    assert_eq!(level, Some(ExplainLevel::Low));
    assert_eq!(text, "Lists a directory.");
}

#[test]
fn multiline_explanation_is_kept() {
    let (level, text) =
        parse_output("LEVEL: medium\nEXPLANATION: First line.\nSecond line with detail.");
    assert_eq!(level, Some(ExplainLevel::Medium));
    assert_eq!(text, "First line.\nSecond line with detail.");
}

#[test]
fn no_explanation_tag_falls_back_to_raw_text() {
    // A chatty model that ignored the format still produces a visible cell.
    let (level, text) = parse_output("LEVEL: low\nThis just prints a greeting.");
    assert_eq!(level, Some(ExplainLevel::Low));
    assert_eq!(text, "This just prints a greeting.");
}

#[test]
fn empty_output_reports_failure() {
    let (level, text) = parse_output("");
    assert_eq!(level, None);
    assert_eq!(text, "explain failed: empty response");

    let (level, text) = parse_output("LEVEL: low\n");
    assert_eq!(level, Some(ExplainLevel::Low));
    assert_eq!(text, "explain failed: empty response");
}

#[test]
fn explanation_is_capped() {
    let long = "x".repeat(EXPLANATION_CAP_CHARS * 3);
    let (level, text) = parse_output(&format!("LEVEL: none\nEXPLANATION: {long}"));
    assert_eq!(level, Some(ExplainLevel::None));
    // truncate_head_tail keeps head+tail under `cap` plus its marker line.
    assert!(text.len() <= EXPLANATION_CAP_CHARS + 64);
    assert!(text.contains("characters truncated"));
}
