use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!(
            "pacode-import-test-instructions-{prefix}-{pid}-{count}"
        ));
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

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn find<'a>(sources: &'a [InstructionSource], rel: &str, root: &Path) -> &'a InstructionSource {
    sources
        .iter()
        .find(|s| s.path == root.join(rel))
        .unwrap_or_else(|| panic!("missing source {rel}"))
}

#[test]
fn test_plain_formats_are_always_on() {
    let temp = TempDir::new("plain");
    let root = temp.path();
    write(root, ".cursorrules", "Use tabs.\n");
    write(root, ".windsurfrules", "Prefer iterators.\n");
    write(root, "GEMINI.md", "# Gemini\nBe brief.\n");
    write(root, ".codex/AGENTS.md", "Run cargo test.\n");
    write(root, ".github/copilot-instructions.md", "Write docs.\n");
    write(root, ".clinerules", "Cline single file.\n");

    let found = discover_instructions(root);
    assert_eq!(found.len(), 6);
    for s in &found {
        assert!(s.always_on, "{:?} should be always on", s.path);
        assert!(s.globs.is_empty());
    }
    assert_eq!(
        find(&found, ".cursorrules", root).kind,
        InstructionKind::CursorRules
    );
    assert_eq!(
        find(&found, ".windsurfrules", root).kind,
        InstructionKind::Windsurf
    );
    assert_eq!(find(&found, "GEMINI.md", root).body, "# Gemini\nBe brief.");
    assert_eq!(
        find(&found, ".codex/AGENTS.md", root).kind,
        InstructionKind::Codex
    );
    assert_eq!(
        find(&found, ".github/copilot-instructions.md", root).kind,
        InstructionKind::CopilotInstructions
    );
    assert_eq!(
        find(&found, ".clinerules", root).kind,
        InstructionKind::ClineRules
    );
}

#[test]
fn test_cursor_mdc_always_apply_and_globs() {
    let temp = TempDir::new("mdc");
    let root = temp.path();
    write(
        root,
        ".cursor/rules/general.mdc",
        "---\ndescription: General\nalwaysApply: true\n---\n\nAlways use Result.\n",
    );
    write(
        root,
        ".cursor/rules/ts.mdc",
        "---\ndescription: TS\nglobs: \"*.ts, *.tsx\"\nalwaysApply: false\n---\nSecret TS body.\n",
    );
    write(
        root,
        ".cursor/rules/rs-list.mdc",
        "---\nglobs:\n  - \"*.rs\"\n  - '*.toml'\n---\nRust body.\n",
    );
    write(root, ".cursor/rules/bare.mdc", "No frontmatter here.\n");
    write(root, ".cursor/rules/ignored.txt", "not a rule\n");

    let found = discover_instructions(root);
    assert_eq!(found.len(), 4);

    let general = find(&found, ".cursor/rules/general.mdc", root);
    assert!(general.always_on);
    assert_eq!(general.body, "Always use Result.");
    assert_eq!(general.kind, InstructionKind::CursorMdc);

    let ts = find(&found, ".cursor/rules/ts.mdc", root);
    assert!(!ts.always_on);
    assert_eq!(ts.globs, vec!["*.ts".to_string(), "*.tsx".to_string()]);
    assert_eq!(ts.body, "Secret TS body.");

    let rs = find(&found, ".cursor/rules/rs-list.mdc", root);
    assert!(!rs.always_on);
    assert_eq!(rs.globs, vec!["*.rs".to_string(), "*.toml".to_string()]);

    let bare = find(&found, ".cursor/rules/bare.mdc", root);
    assert!(bare.always_on, "rules without globs are always on");
    assert_eq!(bare.body, "No frontmatter here.");
}

#[test]
fn test_cline_directory_of_markdown() {
    let temp = TempDir::new("cline-dir");
    let root = temp.path();
    write(root, ".clinerules/b.md", "Second.\n");
    write(root, ".clinerules/a.md", "First.\n");
    write(root, ".clinerules/notes.txt", "skipped\n");

    let found = discover_instructions(root);
    let names: Vec<_> = found
        .iter()
        .map(|s| s.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.md".to_string(), "b.md".to_string()]);
    assert!(
        found
            .iter()
            .all(|s| s.always_on && s.kind == InstructionKind::ClineRules)
    );
}

#[test]
fn test_copilot_scoped_instructions_apply_to() {
    let temp = TempDir::new("copilot");
    let root = temp.path();
    write(
        root,
        ".github/instructions/tests.instructions.md",
        "---\napplyTo: \"**/*_test.go\"\n---\nTable-driven tests.\n",
    );
    write(
        root,
        ".github/instructions/all.instructions.md",
        "---\napplyTo: \"**\"\n---\nAlways-on rule.\n",
    );
    write(root, ".github/instructions/readme.md", "not matched\n");

    let found = discover_instructions(root);
    assert_eq!(found.len(), 2);

    let tests = find(&found, ".github/instructions/tests.instructions.md", root);
    assert!(!tests.always_on);
    assert_eq!(tests.globs, vec!["**/*_test.go".to_string()]);
    assert_eq!(tests.kind, InstructionKind::CopilotScoped);
    assert_eq!(tests.body, "Table-driven tests.");

    let all = find(&found, ".github/instructions/all.instructions.md", root);
    assert!(all.always_on, "applyTo: ** means everywhere");
    assert!(all.globs.is_empty());
}

#[test]
fn test_ancestors_visited_root_to_cwd() {
    let temp = TempDir::new("ancestors");
    let root = temp.path();
    let cwd = root.join("pkg").join("app");
    std::fs::create_dir_all(&cwd).unwrap();
    write(root, "GEMINI.md", "outer\n");
    write(&root.join("pkg"), ".windsurfrules", "middle\n");
    write(&cwd, ".cursorrules", "inner\n");

    let found = discover_instructions(&cwd);
    let bodies: Vec<&str> = found.iter().map(|s| s.body.as_str()).collect();
    assert_eq!(bodies, vec!["outer", "middle", "inner"]);
}

#[test]
fn test_empty_and_missing_files_are_skipped() {
    let temp = TempDir::new("empty");
    let root = temp.path();
    write(root, ".cursorrules", "   \n\n");
    write(root, "GEMINI.md", "  \n\n");
    std::fs::create_dir_all(root.join(".clinerules")).unwrap();

    assert!(discover_instructions(root).is_empty());
}

#[test]
fn test_file_cap_cuts_on_char_boundary() {
    let temp = TempDir::new("file-cap");
    let root = temp.path();
    // Two-byte chars: the byte cap must never leave a split codepoint behind.
    let big = "é".repeat(MAX_FILE_BYTES);
    write(root, ".windsurfrules", &big);

    let found = discover_instructions(root);
    assert_eq!(found.len(), 1);
    assert!(found[0].body.len() <= MAX_FILE_BYTES);
    assert!(found[0].body.chars().all(|c| c == 'é'));
}

#[test]
fn test_total_cap_limits_all_sources() {
    let temp = TempDir::new("total-cap");
    let root = temp.path();
    let chunk = "a".repeat(MAX_FILE_BYTES);
    write(root, "GEMINI.md", &chunk);
    write(root, ".codex/AGENTS.md", &chunk);
    write(root, ".windsurfrules", &chunk);
    write(root, ".cursorrules", &chunk);
    write(root, ".clinerules", &chunk);
    write(root, ".github/copilot-instructions.md", &chunk);

    let found = discover_instructions(root);
    let total: usize = found.iter().map(|s| s.body.len()).sum();
    assert!(total <= MAX_TOTAL_BYTES, "total was {total}");
    assert!(found.len() >= 4);
}

#[test]
fn test_render_always_on_and_scoped_index() {
    let temp = TempDir::new("render");
    let root = temp.path();
    write(root, "GEMINI.md", "Gemini body.\n");
    write(
        root,
        ".cursor/rules/ts.mdc",
        "---\nglobs: \"*.ts\"\n---\nSECRET SCOPED BODY\n",
    );

    let found = discover_instructions(root);
    let rendered = render_prompt_section(&found, root);

    assert!(rendered.contains("# GEMINI.md\n\nGemini body."));
    assert!(rendered.contains(".cursor/rules/ts.mdc [globs: *.ts]"));
    assert!(!rendered.contains("SECRET SCOPED BODY"));
    assert!(!rendered.contains(root.to_string_lossy().as_ref()));
}

#[test]
fn test_render_empty_when_nothing_found() {
    let temp = TempDir::new("render-empty");
    assert_eq!(render_prompt_section(&[], temp.path()), "");
}

#[test]
fn test_frontmatter_without_closing_delimiter_is_body() {
    let text = "---\nalwaysApply: true\nno closing line\n";
    let (meta, body) = parse_frontmatter(text);
    assert_eq!(meta, Meta::default());
    assert_eq!(body, text);
}

#[test]
fn test_truncate_bytes_respects_boundaries() {
    assert_eq!(truncate_bytes("héllo", 2), "h");
    assert_eq!(truncate_bytes("héllo", 3), "hé");
    assert_eq!(truncate_bytes("abc", 10), "abc");
}
