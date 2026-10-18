use std::fs;

use crate::test_util::run_in;

fn write(dir: &tempfile::TempDir, name: &str, content: &[u8]) {
    fs::write(dir.path().join(name), content).expect("write");
}

#[test]
fn identical_files_exit_zero_with_no_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "a.txt", b"x\n");
    write(&dir, "b.txt", b"x\n");
    let out = run_in(dir.path(), "diff", &["a.txt", "b.txt"], None);
    assert_eq!((out.code, out.stdout.len()), (0, 0));
}

#[test]
fn different_files_give_a_unified_diff_and_exit_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "a.txt", b"one\ntwo\nthree\n");
    write(&dir, "b.txt", b"one\n2\nthree\n");
    let out = run_in(dir.path(), "diff", &["-u", "a.txt", "b.txt"], None);
    let text = out.stdout_lossy();
    assert_eq!(out.code, 1);
    assert!(
        text.contains("--- a.txt") && text.contains("+++ b.txt"),
        "{text}"
    );
    assert!(text.contains("-two\n") && text.contains("+2\n"), "{text}");
    assert!(text.contains("@@"), "{text}");
}

#[test]
fn brief_missing_newline_and_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "a.txt", b"a\nb");
    write(&dir, "b.txt", b"a\nc");
    let out = run_in(dir.path(), "diff", &["-q", "a.txt", "b.txt"], None);
    assert_eq!(out.stdout_lossy(), "Files a.txt and b.txt differ\n");
    let out = run_in(dir.path(), "diff", &["a.txt", "b.txt"], None);
    assert!(
        out.stdout_lossy().contains("No newline at end of file"),
        "{}",
        out.stdout_lossy()
    );

    write(&dir, "bin1", b"\0a");
    write(&dir, "bin2", b"\0b");
    let out = run_in(dir.path(), "diff", &["bin1", "bin2"], None);
    assert!(
        out.stdout_lossy().starts_with("Binary files"),
        "{}",
        out.stdout_lossy()
    );
}

#[test]
fn diff_usage_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "a.txt", b"a\n");
    assert_eq!(run_in(dir.path(), "diff", &["a.txt"], None).code, 2);
    assert_eq!(run_in(dir.path(), "diff", &["a.txt", "gone"], None).code, 1);
    assert_eq!(
        run_in(dir.path(), "diff", &["-y", "a.txt", "a.txt"], None).code,
        2
    );
}

#[test]
fn diff_reads_one_side_from_stdin() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "a.txt", b"a\n");
    let out = run_in(dir.path(), "diff", &["a.txt", "-"], Some("b\n"));
    assert_eq!(out.code, 1);
    assert!(out.stdout_lossy().contains("+b"));
}
