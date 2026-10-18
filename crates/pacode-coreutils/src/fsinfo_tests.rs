use std::fs;

use super::{format_utc, human};
use crate::test_util::{ok, run_in, sh};

#[test]
fn basename_strips_directories_and_suffixes() {
    assert_eq!(ok("basename", &["/a/b.txt"], ""), "b.txt\n");
    assert_eq!(ok("basename", &["b.txt", ".txt"], ""), "b\n");
    assert_eq!(ok("basename", &["a/b/"], ""), "b\n");
    assert_eq!(ok("basename", &["/"], ""), "/\n");
    assert_eq!(ok("basename", &["-a", "x/a", "y/b"], ""), "a\nb\n");
    assert_eq!(ok("basename", &["-s", ".rs", "src/main.rs"], ""), "main\n");
    assert_eq!(ok("basename", &[".txt", ".txt"], ""), ".txt\n");
    assert_eq!(sh("basename", &[], None).1, 2);
}

#[test]
fn dirname_returns_the_parent() {
    assert_eq!(ok("dirname", &["/a/b"], ""), "/a\n");
    assert_eq!(ok("dirname", &["a"], ""), ".\n");
    assert_eq!(ok("dirname", &["/a"], ""), "/\n");
    assert_eq!(ok("dirname", &["a/b/"], ""), "a\n");
    assert_eq!(ok("dirname", &["/"], ""), "/\n");
    assert_eq!(ok("dirname", &["a//b"], ""), "a\n");
    assert_eq!(ok("dirname", &["a/b", "c"], ""), "a\n.\n");
}

#[test]
fn realpath_resolves_relative_to_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("f.txt"), "x").expect("write");
    let canonical = dir.path().canonicalize().expect("canonicalize");

    let out = run_in(dir.path(), "realpath", &["f.txt"], None);
    assert_eq!(
        out.stdout_lossy(),
        format!("{}\n", canonical.join("f.txt").display())
    );

    let out = run_in(dir.path(), "realpath", &["missing"], None);
    assert_eq!(
        out.stdout_lossy(),
        format!("{}\n", canonical.join("missing").display())
    );

    let out = run_in(dir.path(), "realpath", &["-e", "missing"], None);
    assert_eq!(out.code, 1);

    let out = run_in(dir.path(), "realpath", &["-m", "a/../b"], None);
    assert_eq!(
        out.stdout_lossy(),
        format!("{}\n", dir.path().join("b").display())
    );
}

#[test]
fn stat_reports_size_type_and_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("f.txt"), "hello").expect("write");
    fs::create_dir(dir.path().join("d")).expect("mkdir");

    let out = run_in(dir.path(), "stat", &["-c", "%s %n %F", "f.txt"], None);
    assert_eq!(out.stdout_lossy(), "5 f.txt regular file\n");
    let out = run_in(dir.path(), "stat", &["-c", "%F", "d"], None);
    assert_eq!(out.stdout_lossy(), "directory\n");

    let out = run_in(dir.path(), "stat", &["f.txt"], None);
    let text = out.stdout_lossy();
    assert!(
        text.contains("File: f.txt") && text.contains("Size: 5"),
        "{text}"
    );

    assert_eq!(run_in(dir.path(), "stat", &["nope"], None).code, 1);
    assert_eq!(
        run_in(dir.path(), "stat", &["-c", "%z", "f.txt"], None).code,
        2
    );
}

#[cfg(unix)]
#[test]
fn stat_mode_bits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f.txt");
    fs::write(&path, "x").expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod");
    let out = run_in(dir.path(), "stat", &["-c", "%a %A", "f.txt"], None);
    assert_eq!(out.stdout_lossy(), "640 -rw-r-----\n");
}

#[test]
fn timestamps_format_as_utc() {
    assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
    assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00 UTC");
    assert_eq!(format_utc(31_536_000 + 3661), "1971-01-01 01:01:01 UTC");
}

#[test]
fn du_lists_files_with_apparent_sizes() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir(dir.path().join("sub")).expect("mkdir");
    fs::write(dir.path().join("sub/a.txt"), vec![b'a'; 1000]).expect("write");
    fs::write(dir.path().join("b.txt"), vec![b'b'; 2000]).expect("write");

    let out = run_in(dir.path(), "du", &["-ab", "."], None);
    let text = out.stdout_lossy();
    assert!(text.contains("1000\t./sub/a.txt\n"), "{text}");
    assert!(text.contains("2000\t./b.txt\n"), "{text}");

    let out = run_in(dir.path(), "du", &["-sb", "."], None);
    let text = out.stdout_lossy();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.ends_with("\t.\n"), "{text}");

    assert_eq!(run_in(dir.path(), "du", &["nope"], None).code, 1);
}

#[test]
fn human_sizes() {
    assert_eq!(human(512), "512");
    assert_eq!(human(1024), "1.0K");
    assert_eq!(human(1536), "1.5K");
    assert_eq!(human(10 * 1024), "10K");
}
