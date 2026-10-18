use crate::test_util::{ok, sh};

#[test]
fn tr_translates_ranges_and_classes() {
    assert_eq!(ok("tr", &["a-z", "A-Z"], "hello"), "HELLO");
    assert_eq!(ok("tr", &["[:lower:]", "[:upper:]"], "abc1"), "ABC1");
    assert_eq!(ok("tr", &["a-c", "x"], "abcd"), "xxxd");
    assert_eq!(ok("tr", &["é", "e"], "café"), "cafe");
    assert_eq!(ok("tr", &["\\n", " "], "a\nb\n"), "a b ");
}

#[test]
fn tr_deletes_and_squeezes() {
    assert_eq!(ok("tr", &["-d", "l"], "hello"), "heo");
    assert_eq!(ok("tr", &["-d", "\\r"], "a\r\nb\r\n"), "a\nb\n");
    assert_eq!(ok("tr", &["-s", " "], "a   b"), "a b");
    assert_eq!(ok("tr", &["-cd", "0-9"], "a1b2\n"), "12");
    assert_eq!(ok("tr", &["-d", "a"], ""), "");
}

#[test]
fn tr_rejects_bad_usage() {
    assert_eq!(sh("tr", &[], Some("a")).1, 2);
    assert_eq!(sh("tr", &["a"], Some("a")).1, 2);
    assert_eq!(sh("tr", &["-d", "a", "b"], Some("a")).1, 2);
    assert_eq!(sh("tr", &["a", "b"], None).1, 2);
    assert_eq!(sh("tr", &["z-a", "b"], Some("a")).1, 2);
}
