use std::path::{Path, PathBuf};

use crate::{SkillError, SkillRegistry, parse_frontmatter};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "pacode_skills_test_{}_{}_{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_frontmatter_parsing_variants() {
    // 1. Standard frontmatter
    let content1 = "---\nname: my-skill\ndescription: A useful skill\n---\n# Header\nBody text";
    let (name, desc, body) = parse_frontmatter(content1, "fallback_dir");
    assert_eq!(name, "my-skill");
    assert_eq!(desc, "A useful skill");
    assert_eq!(body, "# Header\nBody text");

    // 2. Quoted name and description
    let content2 = "---\nname: \"quoted-name\"\ndescription: 'single quoted'\n---\nBody only";
    let (name, desc, body) = parse_frontmatter(content2, "fallback_dir");
    assert_eq!(name, "quoted-name");
    assert_eq!(desc, "single quoted");
    assert_eq!(body, "Body only");

    // 3. Missing name falls back to directory name
    let content3 = "---\ndescription: only description\n---\nHello";
    let (name, desc, body) = parse_frontmatter(content3, "my_dir");
    assert_eq!(name, "my_dir");
    assert_eq!(desc, "only description");
    assert_eq!(body, "Hello");

    // 4. Empty name falls back to directory name
    let content4 = "---\nname:   \ndescription: foo\n---\nHello";
    let (name, desc, body) = parse_frontmatter(content4, "dir_x");
    assert_eq!(name, "dir_x");
    assert_eq!(desc, "foo");
    assert_eq!(body, "Hello");

    // 5. Missing description becomes empty
    let content5 = "---\nname: foo-bar\n---\nContent here";
    let (name, desc, body) = parse_frontmatter(content5, "fallback");
    assert_eq!(name, "foo-bar");
    assert_eq!(desc, "");
    assert_eq!(body, "Content here");

    // 6. Extra keys ignored
    let content6 =
        "---\nversion: 1\nname: foo\nauthor: Alice\ndescription: desc\nextra: true\n---\nBody";
    let (name, desc, body) = parse_frontmatter(content6, "fallback");
    assert_eq!(name, "foo");
    assert_eq!(desc, "desc");
    assert_eq!(body, "Body");

    // 7. No frontmatter at all
    let content7 = "# Just Markdown\nNo frontmatter here.";
    let (name, desc, body) = parse_frontmatter(content7, "raw_dir");
    assert_eq!(name, "raw_dir");
    assert_eq!(desc, "");
    assert_eq!(body, content7);

    // 8. Unclosed frontmatter
    let content8 = "---\nname: broken\ndescription: no closing";
    let (name, desc, body) = parse_frontmatter(content8, "broken_dir");
    assert_eq!(name, "broken_dir");
    assert_eq!(desc, "");
    assert_eq!(body, content8);

    // 9. Windows CRLF line endings
    let content9 = "---\r\nname: crlf-skill\r\ndescription: with crlf\r\n---\r\n# Body\r\nLine 2";
    let (name, desc, body) = parse_frontmatter(content9, "fallback");
    assert_eq!(name, "crlf-skill");
    assert_eq!(desc, "with crlf");
    assert_eq!(body, "# Body\r\nLine 2");
}

#[test]
fn test_dedup_first_wins_and_sorted_by_name() {
    let temp1 = TempDirGuard::new("dedup_root1");
    let temp2 = TempDirGuard::new("dedup_root2");

    // In temp1:
    // - zulu (name: "zulu", desc: "zulu from temp1")
    // - shared (name: "shared", desc: "shared from temp1")
    let zulu_dir = temp1.path().join("zulu");
    std::fs::create_dir_all(&zulu_dir).expect("create zulu dir");
    std::fs::write(
        zulu_dir.join("SKILL.md"),
        "---\nname: zulu\ndescription: zulu from temp1\n---\nZulu body 1",
    )
    .expect("write zulu");

    let shared1_dir = temp1.path().join("shared");
    std::fs::create_dir_all(&shared1_dir).expect("create shared1 dir");
    std::fs::write(
        shared1_dir.join("SKILL.md"),
        "---\nname: shared\ndescription: shared from temp1\n---\nShared body 1",
    )
    .expect("write shared1");

    // In temp2:
    // - alpha (name: "alpha", desc: "alpha from temp2")
    // - shared (name: "shared", desc: "shared from temp2 - should be dropped")
    let alpha_dir = temp2.path().join("alpha");
    std::fs::create_dir_all(&alpha_dir).expect("create alpha dir");
    std::fs::write(
        alpha_dir.join("SKILL.md"),
        "---\nname: alpha\ndescription: alpha from temp2\n---\nAlpha body 2",
    )
    .expect("write alpha");

    let shared2_dir = temp2.path().join("shared");
    std::fs::create_dir_all(&shared2_dir).expect("create shared2 dir");
    std::fs::write(
        shared2_dir.join("SKILL.md"),
        "---\nname: shared\ndescription: shared from temp2\n---\nShared body 2",
    )
    .expect("write shared2");

    let (registry, warnings) =
        SkillRegistry::load(&[temp1.path().to_path_buf(), temp2.path().to_path_buf()]);
    assert!(warnings.is_empty());

    let skills = registry.skills();
    assert_eq!(skills.len(), 3);

    // Check sorted order by name: alpha, shared, zulu
    assert_eq!(skills[0].name, "alpha");
    assert_eq!(skills[1].name, "shared");
    assert_eq!(skills[2].name, "zulu");

    // Check that temp1's "shared" won (first wins)
    assert_eq!(skills[1].description, "shared from temp1");
    let body = registry.body("shared", 1000).expect("read body");
    assert_eq!(body, "Shared body 1");
}

#[test]
fn test_body_truncation_on_char_boundary() {
    let temp = TempDirGuard::new("truncation");
    let skill_dir = temp.path().join("utf8_skill");
    std::fs::create_dir_all(&skill_dir).expect("create dir");

    // Body contains 4-byte UTF-8 emoji: 🦀 (4 bytes: F0 9F A6 80)
    // "prefix " is 7 bytes.
    // Followed by "🦀" (4 bytes) at byte offset 7..11.
    // Followed by " suffix" (7 bytes). Total = 18 bytes.
    let content = "---\nname: utf8-skill\ndescription: test utf8\n---\nprefix 🦀 suffix";
    std::fs::write(skill_dir.join("SKILL.md"), content).expect("write skill");

    let (registry, warnings) = SkillRegistry::load(&[temp.path().to_path_buf()]);
    assert!(warnings.is_empty());

    // 1. Untruncated when max_bytes >= body length
    let full = registry.body("utf8-skill", 100).expect("read full");
    assert_eq!(full, "prefix 🦀 suffix");

    // 2. Cutoff right inside the 4-byte 🦀 (e.g. max_bytes = 8, 9, or 10)
    // Byte 7 is ' ', bytes 7..11 is 🦀.
    // If max_bytes = 8 (which is inside 🦀), it must step back to 7 ("prefix ")
    // and NOT panic with byte index error.
    let truncated_at_8 = registry.body("utf8-skill", 8).expect("truncate at 8");
    assert!(truncated_at_8.starts_with("prefix "));
    assert!(!truncated_at_8.contains('🦀'));
    assert!(truncated_at_8.ends_with("[... truncated ...]"));

    // If max_bytes = 11, it includes 🦀 cleanly
    let truncated_at_11 = registry.body("utf8-skill", 11).expect("truncate at 11");
    assert!(truncated_at_11.starts_with("prefix 🦀"));
    assert!(truncated_at_11.ends_with("[... truncated ...]"));
}

#[test]
fn test_unknown_name_error_lists_available() {
    let temp = TempDirGuard::new("unknown_skill");
    let dir_a = temp.path().join("deploy");
    std::fs::create_dir_all(&dir_a).expect("create dir_a");
    std::fs::write(
        dir_a.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deploy app\n---\nDeploy body",
    )
    .expect("write deploy");

    let dir_b = temp.path().join("lint");
    std::fs::create_dir_all(&dir_b).expect("create dir_b");
    std::fs::write(
        dir_b.join("SKILL.md"),
        "---\nname: lint\ndescription: Run linter\n---\nLint body",
    )
    .expect("write lint");

    let (registry, warnings) = SkillRegistry::load(&[temp.path().to_path_buf()]);
    assert!(warnings.is_empty());

    let err = registry
        .body("nonexistent", 1000)
        .expect_err("should fail for unknown skill");
    match err {
        SkillError::NotFound { name, available } => {
            assert_eq!(name, "nonexistent");
            assert!(available.contains("deploy"));
            assert!(available.contains("lint"));
            let msg = format!(
                "{NotFound}",
                NotFound = SkillError::NotFound { name, available }
            );
            assert!(msg.contains("unknown skill 'nonexistent'"));
            assert!(msg.contains("deploy"));
            assert!(msg.contains("lint"));
        }
        other => panic!("expected NotFound, got {other:?}"),
    }

    // Also test empty registry lists "(none)"
    let empty_reg = SkillRegistry::default();
    let err_empty = empty_reg.body("foo", 100).expect_err("should fail");
    match err_empty {
        SkillError::NotFound { name, available } => {
            assert_eq!(name, "foo");
            assert_eq!(available, "(none)");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn test_unreadable_file_becomes_warning() {
    let temp = TempDirGuard::new("unreadable");
    let broken_dir = temp.path().join("broken");
    std::fs::create_dir_all(&broken_dir).expect("create broken dir");

    // Write invalid non-UTF-8 bytes to SKILL.md
    let skill_path = broken_dir.join("SKILL.md");
    std::fs::write(&skill_path, [0xFF, 0xFE, 0xFD]).expect("write invalid utf8 bytes");

    let (registry, warnings) = SkillRegistry::load(&[temp.path().to_path_buf()]);
    assert_eq!(registry.skills().len(), 0);
    assert_eq!(warnings.len(), 1);
    match &warnings[0] {
        crate::SkillWarning::ReadFailed { path, source } => {
            assert_eq!(path, &skill_path);
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
        }
        other => panic!("expected ReadFailed warning, got {other:?}"),
    }
}
