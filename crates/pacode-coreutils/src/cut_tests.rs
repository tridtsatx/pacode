use crate::test_util::{ok, sh};

#[test]
fn cut_fields_by_delimiter() {
    assert_eq!(ok("cut", &["-d,", "-f2"], "a,b,c\n"), "b\n");
    assert_eq!(ok("cut", &["-d", ",", "-f1,3"], "a,b,c\n"), "a,c\n");
    assert_eq!(ok("cut", &["-d,", "-f2-"], "a,b,c\n"), "b,c\n");
    assert_eq!(ok("cut", &["-f2"], "a\tb\n"), "b\n");
    assert_eq!(ok("cut", &["-d,", "-f2"], "a,b"), "b\n");
    assert_eq!(ok("cut", &["-d,", "-f2"], ""), "");
}

#[test]
fn cut_lines_without_delimiter() {
    assert_eq!(ok("cut", &["-d,", "-f1"], "abc\n"), "abc\n");
    assert_eq!(ok("cut", &["-d,", "-f1", "-s"], "abc\n"), "");
}

#[test]
fn cut_characters_are_unicode_aware() {
    assert_eq!(ok("cut", &["-c1-2"], "héllo\n"), "hé\n");
    assert_eq!(ok("cut", &["-c", "-2,5"], "abcdef\n"), "abe\n");
}

#[test]
fn cut_keeps_carriage_returns_on_the_last_field() {
    assert_eq!(ok("cut", &["-d,", "-f2"], "a,b\r\n"), "b\r\n");
}

#[test]
fn cut_output_delimiter() {
    assert_eq!(
        ok("cut", &["-d,", "-f1,2", "--output-delimiter=:"], "a,b,c\n"),
        "a:b\n"
    );
}

#[test]
fn cut_rejects_bad_usage() {
    assert_eq!(sh("cut", &[], Some("a\n")).1, 2);
    assert_eq!(sh("cut", &["-f1", "-c1"], Some("a\n")).1, 2);
    assert_eq!(sh("cut", &["-f0"], Some("a\n")).1, 2);
    assert_eq!(sh("cut", &["-b1"], Some("a\n")).1, 2);
}
