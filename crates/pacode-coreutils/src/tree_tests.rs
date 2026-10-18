use std::fs;

use super::{Node, last_flags};
use crate::test_util::run_in;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir(dir.path().join("a")).expect("mkdir");
    fs::write(dir.path().join("a/x.txt"), "").expect("write");
    fs::write(dir.path().join("a/y.txt"), "").expect("write");
    fs::write(dir.path().join("b.txt"), "").expect("write");
    fs::write(dir.path().join(".hidden"), "").expect("write");
    fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("write");
    fs::write(dir.path().join("ignored.txt"), "").expect("write");
    dir
}

#[test]
fn tree_draws_branches_and_respects_gitignore() {
    let dir = fixture();
    let out = run_in(dir.path(), "tree", &[], None);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.stdout_lossy(),
        ".\n├── a\n│   ├── x.txt\n│   └── y.txt\n└── b.txt\n\n1 directories, 3 files\n"
    );
}

#[test]
fn tree_depth_and_flags() {
    let dir = fixture();
    let out = run_in(dir.path(), "tree", &["-L", "1", "--noreport"], None);
    assert_eq!(out.stdout_lossy(), ".\n├── a\n└── b.txt\n");

    let out = run_in(dir.path(), "tree", &["-a"], None);
    let text = out.stdout_lossy();
    assert!(text.contains(".hidden"), "{text}");
    assert!(!text.contains("ignored.txt"), "{text}");

    let out = run_in(dir.path(), "tree", &["-d"], None);
    assert!(out.stdout_lossy().contains("└── a\n") || out.stdout_lossy().contains("├── a\n"));
    assert!(!out.stdout_lossy().contains("b.txt"));
}

#[test]
fn tree_of_a_file_or_missing_dir_fails() {
    let dir = fixture();
    assert_eq!(run_in(dir.path(), "tree", &["b.txt"], None).code, 1);
    assert_eq!(run_in(dir.path(), "tree", &["nope"], None).code, 1);
    assert_eq!(run_in(dir.path(), "tree", &["-L", "0"], None).code, 2);
}

#[test]
fn last_sibling_detection() {
    let node = |depth| Node {
        depth,
        name: String::new(),
        is_dir: false,
    };
    let nodes = vec![node(1), node(2), node(2), node(1), node(2)];
    assert_eq!(last_flags(&nodes), vec![false, false, true, true, true]);
}
