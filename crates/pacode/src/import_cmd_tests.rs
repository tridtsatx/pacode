use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pacode_config::Paths;

use super::*;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path =
            std::env::temp_dir().join(format!("pacode-test-import-cmd-{prefix}-{pid}-{count}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_import_dry_run_and_apply() {
    let temp = TempDir::new("full");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let pacode_home = temp.path().join("pacode");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&pacode_home).unwrap();

    let paths = Paths::under(&pacode_home);

    // 1. Set up Claude Code MCP config in home
    let claude_json = r#"{
        "mcpServers": {
            "claude-tool": {
                "command": "node",
                "args": ["server.js"]
            }
        }
    }"#;
    std::fs::write(home.join(".claude.json"), claude_json).unwrap();

    // 2. Set up a skill in home
    let skill_dir = home.join(".claude").join("skills").join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: A useful skill\n---\n# My Skill\n",
    )
    .unwrap();

    // Test DRY RUN (no --apply)
    let dry_args = ImportArgs {
        from: vec!["claude".to_string()],
        mcp: false,
        skills: false,
        apply: false,
        force: false,
    };
    let mut dry_output = Vec::new();
    run_with_dirs(dry_args, &paths, &home, &cwd, &mut dry_output).unwrap();
    let dry_text = String::from_utf8(dry_output).unwrap();

    assert!(dry_text.contains("SOURCE"));
    assert!(dry_text.contains("Claude Code"));
    assert!(dry_text.contains("claude-tool"));
    assert!(dry_text.contains("my-skill"));
    assert!(dry_text.contains("new"));
    assert!(dry_text.contains("Re-run with --apply to import."));

    // Ensure config.toml does NOT exist yet after dry run
    assert!(!paths.config_file.exists());
    assert!(!paths.config_file.parent().unwrap().join("skills").exists());

    // Test APPLY
    let apply_args = ImportArgs {
        from: vec!["claude".to_string()],
        mcp: false,
        skills: false,
        apply: true,
        force: false,
    };
    let mut apply_output = Vec::new();
    run_with_dirs(apply_args, &paths, &home, &cwd, &mut apply_output).unwrap();
    let apply_text = String::from_utf8(apply_output).unwrap();

    assert!(apply_text.contains("Imported mcp 'claude-tool'"));
    assert!(apply_text.contains("Imported skill 'my-skill'"));
    assert!(apply_text.contains("Imported 1 server(s), 1 skill(s)."));

    // Assert resulting config.toml
    assert!(paths.config_file.exists());
    let cfg = pacode_config::load(&paths).expect("saved config must be valid pacode config");
    let srv = cfg
        .mcp
        .servers
        .get("claude-tool")
        .expect("claude-tool server must exist in config");
    assert_eq!(srv.command, "node");
    assert_eq!(srv.args, vec!["server.js"]);

    // Assert copied skill files
    let target_skill_dir = paths
        .config_file
        .parent()
        .unwrap()
        .join("skills")
        .join("my-skill");
    assert!(target_skill_dir.exists());
    assert!(target_skill_dir.join("SKILL.md").exists());
    let skill_content = std::fs::read_to_string(target_skill_dir.join("SKILL.md")).unwrap();
    assert!(skill_content.contains("A useful skill"));

    // Test running dry run AGAIN: rows should now be 'same'
    let mut dry_again_output = Vec::new();
    let dry_again_args = ImportArgs {
        from: vec!["claude".to_string()],
        mcp: false,
        skills: false,
        apply: false,
        force: false,
    };
    run_with_dirs(dry_again_args, &paths, &home, &cwd, &mut dry_again_output).unwrap();
    let dry_again_text = String::from_utf8(dry_again_output).unwrap();
    assert!(dry_again_text.contains("same"));
}

#[test]
fn test_import_filtering_mcp_and_skills_flags() {
    let temp = TempDir::new("filters");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let pacode_home = temp.path().join("pacode");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let paths = Paths::under(&pacode_home);

    std::fs::write(
        home.join(".claude.json"),
        r#"{ "mcpServers": { "srv1": { "command": "echo" } } }"#,
    )
    .unwrap();

    let skill_dir = home.join(".claude").join("skills").join("skill1");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: skill1\n---\n").unwrap();

    // --mcp only
    let mcp_only_args = ImportArgs {
        from: vec!["claude".to_string()],
        mcp: true,
        skills: false,
        apply: false,
        force: false,
    };
    let mut out = Vec::new();
    run_with_dirs(mcp_only_args, &paths, &home, &cwd, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("srv1"));
    assert!(!text.contains("skill1"));

    // --skills only
    let skills_only_args = ImportArgs {
        from: vec!["claude".to_string()],
        mcp: false,
        skills: true,
        apply: false,
        force: false,
    };
    let mut out2 = Vec::new();
    run_with_dirs(skills_only_args, &paths, &home, &cwd, &mut out2).unwrap();
    let text2 = String::from_utf8(out2).unwrap();
    assert!(!text2.contains("srv1"));
    assert!(text2.contains("skill1"));
}
