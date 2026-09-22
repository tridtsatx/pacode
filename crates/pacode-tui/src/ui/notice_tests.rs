//! Error-notice splitting tests — mostly that arbitrary (multi-byte,
//! lossy-decoded) text can never panic the prefix stripper.

use super::*;

/// A `` (U+FFFD, 3 bytes) straddling the `prefix.len()` boundary —
/// `text[..prefix.len()]` would panic on a non-char-boundary index.
#[test]
fn strip_prefix_ci_never_slices_mid_char() {
    // "turn failed:" is 12 bytes; the second `` sits at bytes 11-13.
    let text = "bcdefghi rest";
    assert_eq!(strip_prefix_ci(text, "turn failed:"), None);

    // First char multi-byte, prefix longer than the text.
    assert_eq!(strip_prefix_ci(" x", "turn failed:"), None);

    // Positive path still strips case-insensitively.
    assert_eq!(
        strip_prefix_ci("Turn Failed: boom", "turn failed:"),
        Some(" boom")
    );
}

/// `render_error` is the full pipeline — feed it lossy-decoded output.
#[test]
fn render_error_accepts_replacement_chars() {
    let opts = RenderOptions::new(80, false);
    let lines = render_error("bad utf8  tail", 80, &opts);
    assert!(!lines.is_empty());
    let head: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(head.contains("Bad utf8"), "{head}");
}
