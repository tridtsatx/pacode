use super::*;

#[test]
fn test_reference_detection_rule_email_and_mid_word() {
    assert!(!is_reference_start("user@example.com", 4));
    assert!(!is_reference_start("foo@bar", 3));
    assert!(!is_reference_start("a@b", 1));

    let refs = parse_references("send to user@example.com or foo@bar please");
    assert!(refs.is_empty());
}

#[test]
fn test_reference_detection_rule_start_and_after_whitespace() {
    assert!(is_reference_start("@src/main.rs", 0));
    assert!(is_reference_start("check @src/main.rs", 6));
    assert!(is_reference_start("\n@src/main.rs", 1));
    assert!(is_reference_start("\t@src/main.rs", 1));

    let refs = parse_references("@src/main.rs and @tests/common.rs");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].path, "src/main.rs");
    assert_eq!(refs[0].range, 0..12);
    assert_eq!(refs[1].path, "tests/common.rs");
    assert_eq!(refs[1].range, 17..33);
}

#[test]
fn test_reference_detection_trailing_punctuation() {
    let refs = parse_references("look at @src/main.rs, then @Cargo.toml.");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].path, "src/main.rs");
    assert_eq!(refs[1].path, "Cargo.toml");
}

#[test]
fn test_find_active_query() {
    // Empty query right after @
    let q = find_active_query("check @", 7);
    assert_eq!(
        q,
        Some(AtQuery {
            at_byte_index: 6,
            query: "".to_string(),
        })
    );

    // Partial path
    let q = find_active_query("check @src/ma", 13);
    assert_eq!(
        q,
        Some(AtQuery {
            at_byte_index: 6,
            query: "src/ma".to_string(),
        })
    );

    // Mid-word @ should not trigger active query
    let q = find_active_query("user@domain", 11);
    assert_eq!(q, None);

    // Cursor past whitespace should not trigger active query
    let q = find_active_query("check @src/main.rs now", 22);
    assert_eq!(q, None);
}
