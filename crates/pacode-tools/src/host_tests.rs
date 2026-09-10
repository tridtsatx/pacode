//! Agent-kind discovery tests: frontmatter parsing and `discover_agent_kinds`.

use super::{AgentKindWarning, discover_agent_kinds};

fn write(dir: &std::path::Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

#[test]
fn yaml_frontmatter_full() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "reviewer.md",
        "---\nname: reviewer\ndescription: Reviews code changes\ntools:\n  - read\n  - grep\nmodel: openai/gpt-4o\n---\nYou are a strict reviewer.\n",
    );

    let (kinds, warnings) = discover_agent_kinds(&[tmp.path().to_path_buf()]);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(kinds.len(), 1);
    let kind = &kinds[0];
    assert_eq!(kind.name, "reviewer");
    assert_eq!(kind.description, "Reviews code changes");
    assert_eq!(
        kind.tools,
        Some(vec!["read".to_string(), "grep".to_string()])
    );
    assert_eq!(kind.model.as_deref(), Some("openai/gpt-4o"));
    assert_eq!(kind.prompt, "You are a strict reviewer.");
    assert_eq!(kind.path, tmp.path().join("reviewer.md"));
}

#[test]
fn yaml_inline_tools_and_quoted_scalars() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "doc.md",
        "---\ndescription: \"Writes docs\"\ntools: [read, 'write']\n---\nBody.\n",
    );

    let (kinds, warnings) = discover_agent_kinds(&[tmp.path().to_path_buf()]);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    // `name` falls back to the file stem.
    assert_eq!(kinds[0].name, "doc");
    assert_eq!(kinds[0].description, "Writes docs");
    assert_eq!(
        kinds[0].tools,
        Some(vec!["read".to_string(), "write".to_string()])
    );
}

#[test]
fn toml_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "tester.md",
        "+++\nname = \"tester\"\ndescription = \"Runs tests\"\ntools = [\"bash\", \"read\"]\n+++\nTest everything.\n",
    );

    let (kinds, warnings) = discover_agent_kinds(&[tmp.path().to_path_buf()]);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].name, "tester");
    assert_eq!(
        kinds[0].tools,
        Some(vec!["bash".to_string(), "read".to_string()])
    );
    assert_eq!(kinds[0].prompt, "Test everything.");
}

#[test]
fn files_without_description_or_frontmatter_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "nodesc.md", "---\nname: nodesc\n---\nbody\n");
    write(tmp.path(), "plain.md", "just some notes\n");
    write(tmp.path(), "notes.txt", "---\ndescription: ignored\n---\n");

    let (kinds, warnings) = discover_agent_kinds(&[tmp.path().to_path_buf()]);
    assert!(kinds.is_empty());
    // Both .md files were skipped with a reason; the .txt file is not an agent
    // file at all and produces no warning.
    assert_eq!(warnings.len(), 2);
    assert!(
        warnings
            .iter()
            .all(|w| matches!(w, AgentKindWarning::Invalid { .. }))
    );
}

#[test]
fn project_dir_shadows_global_on_name_clash() {
    let global = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write(
        global.path(),
        "reviewer.md",
        "---\ndescription: global reviewer\n---\nglobal body\n",
    );
    write(
        project.path(),
        "reviewer.md",
        "---\ndescription: project reviewer\n---\nproject body\n",
    );

    let (kinds, warnings) =
        discover_agent_kinds(&[project.path().to_path_buf(), global.path().to_path_buf()]);
    assert!(warnings.is_empty());
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].description, "project reviewer");
    assert_eq!(kinds[0].prompt, "project body");
}

#[test]
fn missing_dir_is_not_a_warning() {
    let (kinds, warnings) =
        discover_agent_kinds(&[std::path::PathBuf::from("/nonexistent-pacode-agents-dir")]);
    assert!(kinds.is_empty());
    assert!(warnings.is_empty());
}
