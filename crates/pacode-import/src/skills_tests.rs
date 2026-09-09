use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::discover;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path =
            std::env::temp_dir().join(format!("pacode-import-test-skills-{prefix}-{pid}-{count}"));
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
fn test_claude_user_and_plugin_skills() {
    let temp = TempDir::new("claude-skills");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");

    // 1. User skill
    let user_skill_dir = home.join(".claude").join("skills").join("commit-helper");
    std::fs::create_dir_all(&user_skill_dir).unwrap();
    let user_skill_md = r#"---
name: "git-commit"
description: 'Generate conventional commit messages'
---

# Commit helper documentation
"#;
    std::fs::write(user_skill_dir.join("SKILL.md"), user_skill_md).unwrap();

    // 2. Plugin skill depth 2 (*/*/skills/*/SKILL.md)
    let plugin_skill_dir_1 = home
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("anthropic")
        .join("code-review")
        .join("skills")
        .join("reviewer");
    std::fs::create_dir_all(&plugin_skill_dir_1).unwrap();
    let plugin_skill_md_1 = r#"---
name: code-review
description: >-
  Reviews code for bugs,
  security issues,
  and formatting.
version: "1.0"
---

# Reviewer docs
"#;
    std::fs::write(plugin_skill_dir_1.join("SKILL.md"), plugin_skill_md_1).unwrap();

    // 3. Plugin skill depth 3 (*/*/*/skills/*/SKILL.md)
    let plugin_skill_dir_2 = home
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("registry")
        .join("tools")
        .join("v2")
        .join("skills")
        .join("linter");
    std::fs::create_dir_all(&plugin_skill_dir_2).unwrap();
    let plugin_skill_md_2 = r#"---
description: Run repository linters
---

# Linter docs
"#;
    std::fs::write(plugin_skill_dir_2.join("SKILL.md"), plugin_skill_md_2).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::ClaudeCode]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.skills.len(), 3);

    let user_skill = disc.skills.iter().find(|s| s.name == "git-commit").unwrap();
    assert_eq!(user_skill.source, ImportSource::ClaudeCode);
    assert_eq!(
        user_skill.description,
        "Generate conventional commit messages"
    );
    assert_eq!(user_skill.dir, user_skill_dir);
    assert_eq!(user_skill.origin, user_skill_dir.join("SKILL.md"));

    let plugin_skill_1 = disc
        .skills
        .iter()
        .find(|s| s.name == "code-review")
        .unwrap();
    assert_eq!(plugin_skill_1.source, ImportSource::ClaudeCode);
    assert_eq!(
        plugin_skill_1.description,
        "Reviews code for bugs, security issues, and formatting."
    );
    assert_eq!(plugin_skill_1.dir, plugin_skill_dir_1);

    // Absent name in frontmatter falls back to directory name ("linter")
    let plugin_skill_2 = disc.skills.iter().find(|s| s.name == "linter").unwrap();
    assert_eq!(plugin_skill_2.source, ImportSource::ClaudeCode);
    assert_eq!(plugin_skill_2.description, "Run repository linters");
    assert_eq!(plugin_skill_2.dir, plugin_skill_dir_2);
}

#[test]
fn test_opencode_skills() {
    let temp = TempDir::new("opencode-skills");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");

    // skill/*
    let skill_dir_1 = home
        .join(".config")
        .join("opencode")
        .join("skill")
        .join("summarizer");
    std::fs::create_dir_all(&skill_dir_1).unwrap();
    let skill_1_md = r#"---
name: summarizer
description: Summarize documents and code
---
"#;
    std::fs::write(skill_dir_1.join("SKILL.md"), skill_1_md).unwrap();

    // skills/*
    let skill_dir_2 = home
        .join(".config")
        .join("opencode")
        .join("skills")
        .join("deployer");
    std::fs::create_dir_all(&skill_dir_2).unwrap();
    let skill_2_md = r#"---
name: deployer
description: Deploy code to cloud environments
---
"#;
    std::fs::write(skill_dir_2.join("SKILL.md"), skill_2_md).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::OpenCode]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.skills.len(), 2);

    let s1 = disc.skills.iter().find(|s| s.name == "summarizer").unwrap();
    assert_eq!(s1.source, ImportSource::OpenCode);
    assert_eq!(s1.description, "Summarize documents and code");
    assert_eq!(s1.dir, skill_dir_1);

    let s2 = disc.skills.iter().find(|s| s.name == "deployer").unwrap();
    assert_eq!(s2.source, ImportSource::OpenCode);
    assert_eq!(s2.description, "Deploy code to cloud environments");
    assert_eq!(s2.dir, skill_dir_2);
}

#[test]
fn test_frontmatter_parsing_variants() {
    // Folded with continuation on following indented lines
    let text1 = r#"---
name: folded-skill
description: This is a folded multi-line description
  that continues on the next line
  and even a third line.
---
Body
"#;
    let (name1, desc1) = parse_skill_frontmatter(text1, "fallback");
    assert_eq!(name1, "folded-skill");
    assert_eq!(
        desc1,
        "This is a folded multi-line description that continues on the next line and even a third line."
    );

    // Literal block scalar with pipe
    let text2 = r#"---
name: literal-skill
description: |
  Line 1
  Line 2
---
"#;
    let (name2, desc2) = parse_skill_frontmatter(text2, "fallback");
    assert_eq!(name2, "literal-skill");
    assert_eq!(desc2, "Line 1\nLine 2");

    // Absent name falls back to dir_name
    let text3 = r#"---
description: Only description here
---
"#;
    let (name3, desc3) = parse_skill_frontmatter(text3, "my-directory");
    assert_eq!(name3, "my-directory");
    assert_eq!(desc3, "Only description here");

    // Absent description falls back to empty string
    let text4 = r#"---
name: only-name
---
"#;
    let (name4, desc4) = parse_skill_frontmatter(text4, "fallback");
    assert_eq!(name4, "only-name");
    assert_eq!(desc4, "");

    // No frontmatter delimiters
    let text5 = r#"# Plain Markdown
No frontmatter at all.
"#;
    let (name5, desc5) = parse_skill_frontmatter(text5, "dir-fallback");
    assert_eq!(name5, "dir-fallback");
    assert_eq!(desc5, "");

    // Quoted strings with colon inside value
    let text6 = r#"---
name: "special:name"
description: "Note: multi-line quoted
  string continuation"
---
"#;
    let (name6, desc6) = parse_skill_frontmatter(text6, "fallback");
    assert_eq!(name6, "special:name");
    assert_eq!(desc6, "Note: multi-line quoted string continuation");
}

#[test]
fn test_unquote_utility() {
    assert_eq!(unquote("plain"), "plain");
    assert_eq!(unquote("\"double quoted\""), "double quoted");
    assert_eq!(unquote("'single quoted'"), "single quoted");
    assert_eq!(unquote("\"escaped \\\"quotes\\\"\""), "escaped \"quotes\"");
    assert_eq!(unquote(""), "");
    assert_eq!(unquote(" "), "");
}
