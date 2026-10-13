use std::path::Path;

use serde_json::json;

use super::BashTool;
use crate::Tool;

fn concurrent(command: &str) -> bool {
    BashTool::default().runs_concurrently(&json!({ "command": command }), Path::new("/work/repo"))
}

#[test]
fn reads_that_need_no_prompt_run_alongside_each_other() {
    for command in [
        "git status --short",
        "git log --oneline -5",
        "ls -la crates",
        "rg -n foo src",
        "wc -l src/main.rs",
        "cat Cargo.toml | head -20",
        "timeout 0.3 tail -f /dev/null; echo done",
    ] {
        assert!(concurrent(command), "{command} only reads");
    }
}

#[test]
fn anything_that_may_write_or_ask_keeps_its_place() {
    for command in [
        "rm -rf build",
        "cargo build",
        "echo hi > out.txt",
        "sed -i s/a/b/ file",
        "git commit -m x",
        "cat .env",
        "cat ../../secret",
        "find . -delete",
        "ls $(whoami)",
        "",
    ] {
        assert!(!concurrent(command), "{command:?} must stay sequential");
    }
    assert!(
        !BashTool::default().runs_concurrently(&json!({}), Path::new("/work")),
        "no command, nothing to prove"
    );
}
