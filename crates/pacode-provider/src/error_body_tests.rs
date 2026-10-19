//! Tests for [`error_body_message`]: JSON error shapes, plain text, HTML, caps.

use super::{append_response_body_chunk, error_body_message, truncate_bytes};

#[test]
fn test_anthropic_error_shape() {
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages: text content is empty"}}"#;
    assert_eq!(
        error_body_message(body),
        "invalid_request_error: messages: text content is empty"
    );
}

#[test]
fn test_openai_error_shape() {
    let body =
        r#"{"error":{"message":"Invalid model specified: foo","type":"invalid_request_error"}}"#;
    assert_eq!(
        error_body_message(body),
        "invalid_request_error: Invalid model specified: foo"
    );
}

#[test]
fn test_error_code_prefixes_when_no_type() {
    let body = r#"{"error":{"code":"invalid_api_key","message":"Incorrect API key"}}"#;
    assert_eq!(
        error_body_message(body),
        "invalid_api_key: Incorrect API key"
    );
}

#[test]
fn test_error_message_without_type() {
    let body = r#"{"error":{"message":"quota exceeded"}}"#;
    assert_eq!(error_body_message(body), "quota exceeded");
}

#[test]
fn test_type_equal_to_message_not_duplicated() {
    let body = r#"{"error":{"type":"oops","message":"oops"}}"#;
    assert_eq!(error_body_message(body), "oops");
}

#[test]
fn test_flat_message() {
    let body = r#"{"message":"model not found"}"#;
    assert_eq!(error_body_message(body), "model not found");
}

#[test]
fn test_error_as_string() {
    let body = r#"{"error":"invalid_grant"}"#;
    assert_eq!(error_body_message(body), "invalid_grant");
}

#[test]
fn test_error_string_with_description() {
    let body = r#"{"error":"invalid_grant","error_description":"Session expired"}"#;
    assert_eq!(error_body_message(body), "invalid_grant: Session expired");
}

#[test]
fn test_detail_fallback() {
    let body = r#"{"detail":"Not authenticated"}"#;
    assert_eq!(error_body_message(body), "Not authenticated");
}

#[test]
fn test_error_description_fallback() {
    let body = r#"{"error":{"description":"token revoked"}}"#;
    assert_eq!(error_body_message(body), "token revoked");
}

#[test]
fn test_plain_text_passthrough() {
    assert_eq!(error_body_message("rate limited"), "rate limited");
}

#[test]
fn test_html_page_capped_not_parsed() {
    let body = "<html><body><h1>502 Bad Gateway</h1><p>nginx</p></body></html>";
    assert_eq!(error_body_message(body), body);
}

#[test]
fn test_empty_body() {
    assert_eq!(error_body_message(""), "(empty response body)");
    assert_eq!(error_body_message("   \n\t "), "(empty response body)");
}

#[test]
fn test_json_array_returns_raw() {
    let body = r#"[{"error":{"message":"hidden"}}]"#;
    assert_eq!(error_body_message(body), body);
}

#[test]
fn test_json_string_unquoted() {
    assert_eq!(error_body_message("\"plain string\""), "plain string");
}

#[test]
fn test_json_scalar_returns_raw() {
    assert_eq!(error_body_message("42"), "42");
    assert_eq!(error_body_message("null"), "null");
}

#[test]
fn test_non_string_error_message_level_skipped() {
    // `error.message` being an object must not be serialized — the next
    // fallback level (`detail` here) wins instead.
    let body = r#"{"error":{"message":{"nested":true}},"detail":"readable"}"#;
    assert_eq!(error_body_message(body), "readable");
}

#[test]
fn test_unknown_json_object_returns_raw() {
    let body = r#"{"foo":"bar","n":1}"#;
    assert_eq!(error_body_message(body), body);
}

#[test]
fn test_long_body_capped_at_char_boundary() {
    let body = "x".repeat(500);
    let out = error_body_message(&body);
    assert_eq!(out.chars().count(), 301);
    assert!(out.ends_with('…'));

    // Multibyte chars must not be split.
    let body = "ю".repeat(400);
    let out = error_body_message(&body);
    assert_eq!(out.chars().count(), 301);
    assert!(out.ends_with('…'));
}

#[test]
fn test_long_extracted_message_capped() {
    let long = "y".repeat(400);
    let body = format!(r#"{{"error":{{"message":"{long}"}}}}"#);
    let out = error_body_message(&body);
    assert_eq!(out.chars().count(), 301);
    assert!(out.ends_with('…'));
}

#[test]
fn test_extracted_message_is_trimmed() {
    let body = r#"{"error":{"message":"  spaced out  "}}"#;
    assert_eq!(error_body_message(body), "spaced out");
}

#[test]
fn test_response_body_bytes_are_bounded() {
    let mut body = Vec::new();
    append_response_body_chunk(&mut body, b"abc", 5);
    append_response_body_chunk(&mut body, b"defghij", 5);
    assert_eq!(body, b"abcde");
    assert!(truncate_bytes("ю", 1).is_empty());
}
