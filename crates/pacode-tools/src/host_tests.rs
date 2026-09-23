//! Agent-kind discovery tests: frontmatter parsing and `discover_agent_kinds`,
//! plus `require_permission`'s deny-reason wording.

use std::sync::Arc;

use pacode_types::PermissionDecision;

use super::{AgentKindWarning, discover_agent_kinds};
use crate::test_support::{StubToolHost, stub_ctx};

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

#[tokio::test]
async fn require_permission_allow_returns_decision() {
    let host = Arc::new(StubToolHost::default());
    let ctx = stub_ctx(host);
    let decision = ctx
        .require_permission("Bash: ls", "ls", None)
        .await
        .unwrap();
    assert_eq!(decision, PermissionDecision::AllowOnce);
}

#[tokio::test]
async fn require_permission_policy_deny_quotes_reason_and_forbids_retry() {
    let host = Arc::new(StubToolHost::default());
    *host.decision.lock().unwrap() = PermissionDecision::Deny;
    *host.reason.lock().unwrap() =
        Some("catastrophic operations are denied by policy.".to_string());
    let ctx = stub_ctx(host);

    let err = ctx
        .require_permission("Bash: rm -rf ~", "rm -rf ~", None)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("catastrophic operations are denied by policy"),
        "policy reason must reach the model: {msg}"
    );
    assert!(
        msg.contains("do not retry"),
        "policy denials must be marked non-retryable: {msg}"
    );
    assert!(
        !msg.contains("user denied"),
        "a policy deny must not read as a user refusal: {msg}"
    );
}

#[tokio::test]
async fn require_permission_user_deny_keeps_refusal_wording() {
    let host = Arc::new(StubToolHost::default());
    *host.decision.lock().unwrap() = PermissionDecision::Deny;
    let ctx = stub_ctx(host);

    let err = ctx
        .require_permission("Bash: ls", "ls", None)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "denied: user denied the request");
}
