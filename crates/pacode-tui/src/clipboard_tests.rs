use super::*;

#[test]
fn test_clipboard_base64_encode() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
}

#[test]
fn test_clipboard_osc52_sequence_bytes_known_string() {
    let bytes = osc52_sequence("hello", false);
    // Base64 of "hello" is "aGVsbG8="
    assert_eq!(bytes, b"\x1b]52;c;aGVsbG8=\x07");
}

#[test]
fn test_clipboard_osc52_sequence_tmux() {
    let bytes = osc52_sequence("hello", true);
    // Wrapped in tmux passthrough: \x1bPtmux;\x1b<seq>\x1b\
    assert_eq!(bytes, b"\x1bPtmux;\x1b\x1b]52;c;aGVsbG8=\x07\x1b\\");
}

#[test]
fn test_clipboard_remote_hint_detection() {
    // None set
    assert_eq!(detect_remote_hint(None, None, None), None);
    assert_eq!(detect_remote_hint(Some(""), Some("   "), Some("\t")), None);

    // TMUX set
    assert_eq!(
        detect_remote_hint(Some("/tmp/tmux-1000/default,1234,0"), None, None),
        Some(REMOTE_HINT_TEXT)
    );

    // SSH_TTY set
    assert_eq!(
        detect_remote_hint(None, Some("/dev/pts/2"), None),
        Some(REMOTE_HINT_TEXT)
    );

    // SSH_CONNECTION set
    assert_eq!(
        detect_remote_hint(None, None, Some("192.168.1.5 54321 192.168.1.1 22")),
        Some(REMOTE_HINT_TEXT)
    );

    // Multiple set
    assert_eq!(
        detect_remote_hint(Some("tmux"), Some("/dev/pts/1"), None),
        Some(REMOTE_HINT_TEXT)
    );
}

#[test]
fn test_clipboard_remote_hint_real_env() {
    // Thin wrapper runs without panic regardless of environment
    let _ = remote_hint();
}
