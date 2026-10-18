use super::bre_to_rust;
use crate::test_util::{ok, run_in, sh};

#[test]
fn substitute_first_and_global() {
    assert_eq!(ok("sed", &["s/a/b/"], "banana\n"), "bbnana\n");
    assert_eq!(ok("sed", &["s/a/b/g"], "banana\n"), "bbnbnb\n");
    assert_eq!(ok("sed", &["s/a/b/"], ""), "");
}

#[test]
fn substitute_with_groups_and_ampersand() {
    assert_eq!(
        ok("sed", &["-E", "s/(a)(n)/\\2\\1/"], "banana\n"),
        "bnaana\n"
    );
    assert_eq!(
        ok("sed", &["-E", "s/(a)(n)/\\2\\1/g"], "banana\n"),
        "bnanaa\n"
    );
    assert_eq!(ok("sed", &["s/b/[&]/"], "abc\n"), "a[b]c\n");
    assert_eq!(ok("sed", &["s/b/\\&/"], "abc\n"), "a&c\n");
}

#[test]
fn basic_regex_syntax_is_translated() {
    assert_eq!(
        ok("sed", &["s/\\(a\\)\\(n\\)/\\2\\1/"], "banana\n"),
        "bnaana\n"
    );
    assert_eq!(ok("sed", &["s/(x)/[&]/"], "f(x)\n"), "f[(x)]\n");
    assert_eq!(bre_to_rust("a\\+b"), "a+b");
    assert_eq!(bre_to_rust("a+b"), "a\\+b");
    assert_eq!(bre_to_rust("[a(]"), "[a(]");
}

#[test]
fn delimiters_flags_and_unicode() {
    assert_eq!(ok("sed", &["s|/|-|"], "a/b\n"), "a-b\n");
    assert_eq!(ok("sed", &["s/\\//-/"], "a/b\n"), "a-b\n");
    assert_eq!(ok("sed", &["s/A/x/I"], "a\n"), "x\n");
    assert_eq!(ok("sed", &["s/é/e/g"], "café\n"), "cafe\n");
    assert_eq!(ok("sed", &["s/a/b/"], "a\r\n"), "b\r\n");
    assert_eq!(ok("sed", &["s/b/X/"], "a\nb"), "a\nX");
}

#[test]
fn line_and_range_addresses() {
    assert_eq!(ok("sed", &["-n", "2p"], "a\nb\nc\n"), "b\n");
    assert_eq!(ok("sed", &["-n", "2,3p"], "a\nb\nc\nd\n"), "b\nc\n");
    assert_eq!(ok("sed", &["2d"], "a\nb\nc\n"), "a\nc\n");
    assert_eq!(ok("sed", &["$d"], "a\nb\nc\n"), "a\nb\n");
    assert_eq!(ok("sed", &["/b/d"], "a\nb\nc\n"), "a\nc\n");
    assert_eq!(ok("sed", &["1!d"], "a\nb\n"), "a\n");
    assert_eq!(ok("sed", &["/a/,/c/d"], "x\na\nb\nc\ny\n"), "x\ny\n");
    assert_eq!(ok("sed", &["2s/b/X/"], "b\nb\n"), "b\nX\n");
}

#[test]
fn print_flag_and_quiet() {
    assert_eq!(ok("sed", &["-n", "s/a/b/p"], "a\nc\n"), "b\n");
    assert_eq!(ok("sed", &["p"], "a\n"), "a\na\n");
}

#[test]
fn several_commands() {
    assert_eq!(ok("sed", &["-e", "s/a/b/", "-e", "s/b/c/"], "a\n"), "c\n");
    assert_eq!(ok("sed", &["s/a/b/;s/b/c/"], "a\n"), "c\n");
}

#[test]
fn sed_reads_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("f"), "a\nb\n").expect("write");
    let out = run_in(dir.path(), "sed", &["-n", "2p", "f"], None);
    assert_eq!(out.stdout_lossy(), "b\n");
}

#[test]
fn unsupported_things_exit_two() {
    assert_eq!(sh("sed", &["y/a/b/"], Some("a\n")).1, 2);
    assert_eq!(sh("sed", &["q"], Some("a\n")).1, 2);
    assert_eq!(sh("sed", &["-i", "s/a/b/", "f"], Some("a\n")).1, 2);
    assert_eq!(sh("sed", &["s/a/b/x"], Some("a\n")).1, 2);
    assert_eq!(sh("sed", &[], Some("a\n")).1, 2);
    assert_eq!(sh("sed", &["s/(/x/"], Some("a\n")).1, 0);
    assert_eq!(sh("sed", &["-E", "s/(/x/"], Some("a\n")).1, 2);
    assert_eq!(sh("sed", &["s/a"], Some("a\n")).1, 2);
}
