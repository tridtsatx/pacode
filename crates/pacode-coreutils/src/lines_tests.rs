use std::fs;

use crate::test_util::{ok, run_in, sh};

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("{i}\n")).collect()
}

#[test]
fn wc_counts_lines_words_and_bytes() {
    assert_eq!(ok("wc", &[], "a b\nc\n"), "2 3 6\n");
    assert_eq!(ok("wc", &["-l"], ""), "0\n");
    assert_eq!(ok("wc", &["-l"], "abc"), "0\n");
    assert_eq!(ok("wc", &["-c"], "abc"), "3\n");
}

#[test]
fn wc_counts_unicode_chars_and_bytes_separately() {
    assert_eq!(ok("wc", &["-m"], "héllo wörld"), "11\n");
    assert_eq!(ok("wc", &["-c"], "héllo wörld"), "13\n");
    assert_eq!(ok("wc", &["-w"], "héllo wörld"), "2\n");
}

#[test]
fn wc_handles_crlf() {
    assert_eq!(ok("wc", &["-l"], "a\r\nb\r\n"), "2\n");
    assert_eq!(ok("wc", &["-L"], "a\r\nb\r\n"), "2\n");
}

#[test]
fn wc_over_files_prints_names_and_total() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "x\ny\n").expect("write");
    fs::write(dir.path().join("b.txt"), "z\n").expect("write");
    let out = run_in(dir.path(), "wc", &["-l", "a.txt", "b.txt"], None);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout_lossy(), "2 a.txt\n1 b.txt\n3 total\n");
}

#[test]
fn wc_missing_file_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run_in(dir.path(), "wc", &["nope.txt"], None);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("No such file"), "{}", out.stderr);
}

#[test]
fn head_takes_the_first_lines() {
    assert_eq!(ok("head", &["-n", "2"], "a\nb\nc\n"), "a\nb\n");
    assert_eq!(ok("head", &["-2"], "a\nb\nc"), "a\nb\n");
    assert_eq!(ok("head", &["-n", "-1"], "a\nb\nc\n"), "a\nb\n");
    assert_eq!(ok("head", &[], &numbered(12)), numbered(10));
}

#[test]
fn head_preserves_a_missing_final_newline() {
    assert_eq!(ok("head", &["-n", "5"], "a\nb"), "a\nb");
}

#[test]
fn head_bytes_respect_utf8_input_by_byte_count() {
    assert_eq!(ok("head", &["-c", "3"], "héllo"), "hé");
}

#[test]
fn head_and_tail_of_nothing_are_empty() {
    assert_eq!(ok("head", &[], ""), "");
    assert_eq!(ok("tail", &[], ""), "");
}

#[test]
fn tail_takes_the_last_lines() {
    assert_eq!(ok("tail", &["-n", "2"], "a\nb\nc\n"), "b\nc\n");
    assert_eq!(ok("tail", &["-n", "2"], "a\nb\nc"), "b\nc");
    assert_eq!(ok("tail", &["-n", "+2"], "a\nb\nc\n"), "b\nc\n");
    assert_eq!(ok("tail", &["-n", "0"], "a\n"), "");
    assert_eq!(ok("tail", &["-n", "5"], "a\nb\n"), "a\nb\n");
    assert_eq!(ok("tail", &["-1"], "a\r\nb\r\n"), "b\r\n");
}

#[test]
fn head_with_several_files_prints_headers() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "1\n").expect("write");
    fs::write(dir.path().join("b.txt"), "2\n").expect("write");
    let out = run_in(dir.path(), "head", &["-n1", "a.txt", "b.txt"], None);
    assert_eq!(out.stdout_lossy(), "==> a.txt <==\n1\n\n==> b.txt <==\n2\n");
}

#[test]
fn unsupported_flags_exit_two_instead_of_being_ignored() {
    let out = run_in(std::path::Path::new("."), "head", &["-x"], Some("a\n"));
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("unsupported option -x"),
        "{}",
        out.stderr
    );
    assert_eq!(sh("tail", &["-f"], Some("a\n")).1, 2);
}

#[test]
fn tac_reverses_lines() {
    assert_eq!(ok("tac", &[], "a\nb\nc\n"), "c\nb\na\n");
    assert_eq!(ok("tac", &[], "a\nb"), "b\na\n");
    assert_eq!(ok("tac", &[], ""), "");
}

#[test]
fn rev_reverses_characters() {
    assert_eq!(ok("rev", &[], "abc\nhéllo\n"), "cba\nolléh\n");
    assert_eq!(ok("rev", &[], "ab\r\n"), "\rba\n");
    assert_eq!(ok("rev", &[], ""), "");
}

#[test]
fn nl_numbers_non_empty_lines_by_default() {
    assert_eq!(ok("nl", &[], "a\n\nb\n"), "     1\ta\n       \n     2\tb\n");
    assert_eq!(
        ok("nl", &["-b", "a"], "a\n\nb\n"),
        "     1\ta\n     2\t\n     3\tb\n"
    );
    assert_eq!(ok("nl", &["-n", "rz", "-w", "3"], "x\n"), "001\tx\n");
    assert_eq!(ok("nl", &[], ""), "");
}

#[test]
fn tools_without_input_report_usage() {
    assert_eq!(sh("wc", &[], None).1, 2);
    assert_eq!(sh("tac", &[], None).1, 2);
}
