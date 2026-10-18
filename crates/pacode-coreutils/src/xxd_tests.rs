use crate::test_util::{ok, sh};

#[test]
fn xxd_dumps_hex_and_ascii() {
    assert_eq!(
        ok("xxd", &[], "Hello\n"),
        format!("00000000: {:<39}  Hello.\n", "4865 6c6c 6f0a")
    );
    assert_eq!(ok("xxd", &[], ""), "");
}

#[test]
fn xxd_wraps_rows_and_honours_length() {
    let out = ok("xxd", &[], &"a".repeat(20));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].starts_with("00000010: "), "{}", lines[1]);
    assert_eq!(
        ok("xxd", &["-l", "2"], "Hello"),
        format!("00000000: {:<39}  He\n", "4865")
    );
}

#[test]
fn xxd_plain_mode_and_unsupported_flags() {
    assert_eq!(ok("xxd", &["-p"], "Hello"), "48656c6c6f\n");
    assert_eq!(sh("xxd", &["-r"], Some("00")).1, 2);
}
