use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use pacode_types::config::McpServerConfig;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path =
            std::env::temp_dir().join(format!("pacode-import-test-apply-{prefix}-{pid}-{count}"));
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
fn test_apply_mcp_new_and_same() {
    let temp = TempDir::new("mcp-apply");
    let config_file = temp.path().join("config.toml");
    let skills_dir = temp.path().join("skills");
    let target = ApplyTarget::new(&config_file, &skills_dir, false);

    let mcp = DiscoveredMcp {
        source: ImportSource::ClaudeCode,
        origin: temp.path().join(".claude.json"),
        name: "test-server".to_string(),
        server: McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y", "test-tool"]
                .into_iter()
                .map(String::from)
                .collect(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            lazy: true,
            timeout_secs: 60,
        },
    };

    let plan = vec![PlanEntry::mcp(mcp.clone())];

    // Status before apply should be New
    assert_eq!(check_status(&plan[0], &target), EntryStatus::New);

    // Apply first time: should import
    let report = apply(&plan, &target);
    assert_eq!(report.imported_servers, 1);
    assert_eq!(report.skipped_servers, 0);
    assert_eq!(report.errors.len(), 0);
    assert_eq!(report.entries[0].outcome, ApplyOutcome::Imported);

    // Status after apply should be Same
    assert_eq!(check_status(&plan[0], &target), EntryStatus::Same);

    // Apply second time without force: should skip as Same
    let report2 = apply(&plan, &target);
    assert_eq!(report2.imported_servers, 0);
    assert_eq!(report2.skipped_servers, 1);
    assert_eq!(report2.entries[0].outcome, ApplyOutcome::SkippedSame);

    // Verify config.toml content
    let saved_config = load_existing_config(&config_file).expect("must parse saved config");
    let srv = saved_config
        .mcp
        .servers
        .get("test-server")
        .expect("server must exist");
    assert_eq!(srv.command, "npx");
    assert_eq!(srv.args, vec!["-y", "test-tool"]);
}

#[test]
fn test_apply_mcp_conflict_and_force() {
    let temp = TempDir::new("mcp-conflict");
    let config_file = temp.path().join("config.toml");
    let skills_dir = temp.path().join("skills");

    // Write existing config with a different command
    let initial_toml = r#"
[mcp.servers.server1]
command = "python"
args = ["old.py"]
enabled = true
lazy = true
"#;
    std::fs::write(&config_file, initial_toml).unwrap();

    let mcp = DiscoveredMcp {
        source: ImportSource::OpenCode,
        origin: temp.path().join("opencode.json"),
        name: "server1".to_string(),
        server: McpServerConfig {
            command: "node".to_string(),
            args: vec!["new.js".to_string()],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            lazy: true,
            timeout_secs: 60,
        },
    };

    let plan = vec![PlanEntry::mcp(mcp)];

    let target_no_force = ApplyTarget::new(&config_file, &skills_dir, false);
    assert_eq!(
        check_status(&plan[0], &target_no_force),
        EntryStatus::Conflict
    );

    // Dry or apply without force: skipped as conflict
    let report = apply(&plan, &target_no_force);
    assert_eq!(report.imported_servers, 0);
    assert_eq!(report.skipped_servers, 1);
    assert_eq!(report.entries[0].outcome, ApplyOutcome::SkippedConflict);

    // Verify config was NOT modified
    let cfg = load_existing_config(&config_file).unwrap();
    assert_eq!(cfg.mcp.servers["server1"].command, "python");

    // Apply with force: overwritten
    let target_force = ApplyTarget::new(&config_file, &skills_dir, true);
    let report_force = apply(&plan, &target_force);
    assert_eq!(report_force.imported_servers, 1);
    assert_eq!(report_force.skipped_servers, 0);
    assert_eq!(report_force.entries[0].outcome, ApplyOutcome::Imported);

    let cfg_updated = load_existing_config(&config_file).unwrap();
    assert_eq!(cfg_updated.mcp.servers["server1"].command, "node");
    assert_eq!(cfg_updated.mcp.servers["server1"].args, vec!["new.js"]);
}

#[test]
fn test_apply_skill_new_same_conflict() {
    let temp = TempDir::new("skill-apply");
    let config_file = temp.path().join("config.toml");
    let skills_dir = temp.path().join("skills");
    let source_dir = temp.path().join("source-skill");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("SKILL.md"), "# Test Skill\nContent here").unwrap();
    std::fs::write(source_dir.join("run.sh"), "#!/bin/bash\necho ok").unwrap();

    let skill = DiscoveredSkill {
        source: ImportSource::ClaudeCode,
        origin: source_dir.join("SKILL.md"),
        name: "my-skill".to_string(),
        description: "Test skill description".to_string(),
        dir: source_dir.clone(),
    };

    let plan = vec![PlanEntry::skill(skill.clone())];
    let target = ApplyTarget::new(&config_file, &skills_dir, false);

    // Initial check: New
    assert_eq!(check_status(&plan[0], &target), EntryStatus::New);

    // Apply: should copy
    let report = apply(&plan, &target);
    assert_eq!(report.imported_skills, 1);
    assert_eq!(report.skipped_skills, 0);

    let installed_skill = skills_dir.join("my-skill");
    assert!(installed_skill.join("SKILL.md").exists());
    assert!(installed_skill.join("run.sh").exists());
    assert_eq!(
        std::fs::read_to_string(installed_skill.join("SKILL.md")).unwrap(),
        "# Test Skill\nContent here"
    );

    // Status after copy: Same
    assert_eq!(check_status(&plan[0], &target), EntryStatus::Same);

    // Apply second time: SkippedSame
    let report2 = apply(&plan, &target);
    assert_eq!(report2.imported_skills, 0);
    assert_eq!(report2.skipped_skills, 1);
    assert_eq!(report2.entries[0].outcome, ApplyOutcome::SkippedSame);

    // Modify installed skill: becomes Conflict
    std::fs::write(installed_skill.join("SKILL.md"), "# Modified Skill").unwrap();
    assert_eq!(check_status(&plan[0], &target), EntryStatus::Conflict);

    // Apply without force: SkippedConflict
    let report3 = apply(&plan, &target);
    assert_eq!(report3.imported_skills, 0);
    assert_eq!(report3.skipped_skills, 1);
    assert_eq!(report3.entries[0].outcome, ApplyOutcome::SkippedConflict);

    // Apply with force: overwrites
    let target_force = ApplyTarget::new(&config_file, &skills_dir, true);
    let report4 = apply(&plan, &target_force);
    assert_eq!(report4.imported_skills, 1);
    assert_eq!(
        std::fs::read_to_string(installed_skill.join("SKILL.md")).unwrap(),
        "# Test Skill\nContent here"
    );
}
