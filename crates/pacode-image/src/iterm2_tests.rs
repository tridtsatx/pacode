use super::*;

#[test]
fn test_iterm2_format() {
    let raw = b"test-image-bytes";
    let esc = encode_iterm2(raw, 40, 20);

    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let expected = format!(
        "\x1b]1337;File=inline=1;width=40;height=20;preserveAspectRatio=1:{expected_b64}\x07"
    );
    assert_eq!(esc, expected);
}
