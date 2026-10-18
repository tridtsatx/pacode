use std::fs;

use crate::test_util::{ok, run_in, sh};

fn two_files() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "1\n2\n").expect("write");
    fs::write(dir.path().join("b.txt"), "x\ny\nz\n").expect("write");
    dir
}

#[test]
fn paste_zips_files_and_pads_short_ones() {
    let dir = two_files();
    let out = run_in(dir.path(), "paste", &["a.txt", "b.txt"], None);
    assert_eq!(out.stdout_lossy(), "1\tx\n2\ty\n\tz\n");
    let out = run_in(dir.path(), "paste", &["-d,", "a.txt", "b.txt"], None);
    assert_eq!(out.stdout_lossy(), "1,x\n2,y\n,z\n");
}

#[test]
fn paste_serial_and_stdin_pairs() {
    let dir = two_files();
    let out = run_in(dir.path(), "paste", &["-s", "a.txt"], None);
    assert_eq!(out.stdout_lossy(), "1\t2\n");
    assert_eq!(ok("paste", &["-", "-"], "1\n2\n3\n4\n"), "1\t2\n3\t4\n");
    assert_eq!(ok("paste", &[], ""), "");
}

#[test]
fn join_is_an_inner_join_in_left_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("l.txt"), "1 a\n2 b\n3 c\n").expect("write");
    fs::write(dir.path().join("r.txt"), "2 x\n1 y\n").expect("write");
    let out = run_in(dir.path(), "join", &["l.txt", "r.txt"], None);
    assert_eq!(out.stdout_lossy(), "1 a y\n2 b x\n");
}

#[test]
fn join_with_delimiter_and_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("l.txt"), "a,1\nb,2\n").expect("write");
    fs::write(dir.path().join("r.txt"), "9,b\n8,a\n").expect("write");
    let out = run_in(
        dir.path(),
        "join",
        &["-t", ",", "-2", "2", "l.txt", "r.txt"],
        None,
    );
    assert_eq!(out.stdout_lossy(), "a,1,8\nb,2,9\n");
}

#[test]
fn join_needs_two_inputs() {
    assert_eq!(sh("join", &["only"], None).1, 2);
}
