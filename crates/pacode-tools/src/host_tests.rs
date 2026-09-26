//! Agent-kind discovery tests: frontmatter parsing and `discover_agent_kinds`,
//! plus `require_permission`'s deny-reason wording.

use std::sync::Arc;

use pacode_types::PermissionDecision;

use super::{AgentKindWarning, discover_agent_kinds};
use crate::test_support::{StubToolHost, stub_ctx};

fn write(dir: &std::path::Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

/// The kinds that came from files, as opposed to the built-ins discovery always
/// seeds. Tests about a file assert *their* kind, never the total: the total
/// legitimately includes the built-ins and is not what those tests are about.
fn from_files(kinds: &[super::AgentKindDef]) -> Vec<&super::AgentKindDef> {
    kinds
        .iter()
        .filter(|k| !k.path.to_string_lossy().starts_with("<builtin>"))
        .collect()
}

fn find<'a>(kinds: &'a [super::AgentKindDef], name: &str) -> &'a super::AgentKindDef {
    kinds
        .iter()
        .find(|k| k.name == name)
        .unwrap_or_else(|| panic!("no kind named {name}"))
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
    assert_eq!(
        from_files(&kinds).len(),
        1,
        "exactly one file kind: {kinds:?}"
    );
    let kind = find(&kinds, "reviewer");
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
    assert_eq!(
        from_files(&kinds).len(),
        1,
        "exactly one file kind: {kinds:?}"
    );
    // By name, never by position: the built-ins are sorted in alongside, so an
    // index no longer means "the kind this test wrote".
    let tester = find(&kinds, "tester");
    assert_eq!(tester.name, "tester");
    assert_eq!(
        tester.tools,
        Some(vec!["bash".to_string(), "read".to_string()])
    );
    assert_eq!(tester.prompt, "Test everything.");
}

#[test]
fn files_without_description_or_frontmatter_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "nodesc.md", "---\nname: nodesc\n---\nbody\n");
    write(tmp.path(), "plain.md", "just some notes\n");
    write(tmp.path(), "notes.txt", "---\ndescription: ignored\n---\n");

    let (kinds, warnings) = discover_agent_kinds(&[tmp.path().to_path_buf()]);
    assert!(
        from_files(&kinds).is_empty(),
        "no file produced a kind: {kinds:?}"
    );
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
    assert_eq!(
        kinds.iter().filter(|k| k.name == "reviewer").count(),
        1,
        "one reviewer, not two: {kinds:?}"
    );
    assert_eq!(find(&kinds, "reviewer").description, "project reviewer");
    assert_eq!(find(&kinds, "reviewer").prompt, "project body");
}

#[test]
fn missing_dir_is_not_a_warning() {
    let (kinds, warnings) =
        discover_agent_kinds(&[std::path::PathBuf::from("/nonexistent-pacode-agents-dir")]);
    // A missing directory is normal, not a failure — and the built-ins are
    // still there, which is the point: a fresh install is never kind-less.
    assert!(from_files(&kinds).is_empty(), "no file kinds: {kinds:?}");
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

/// A fresh install must not be kind-less. With nothing on offer, a model asked
/// for a subagent reaches for a plausible name and every such call was a
/// refusal — which is what made "subagents are slow to start" true in practice.
#[test]
fn built_in_kinds_exist_without_any_user_files() {
    let empty = tempfile::tempdir().unwrap();
    let (kinds, warnings) = discover_agent_kinds(&[empty.path().to_path_buf()]);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let explore = find(&kinds, "explore");
    let implement = find(&kinds, "implement");
    assert!(
        !explore.prompt.is_empty() && !implement.prompt.is_empty(),
        "a kind with no brief tells the subagent nothing about its role"
    );
    assert!(
        explore.path.to_string_lossy().starts_with("<builtin>"),
        "built-ins are marked as such so a user file can be told apart"
    );
}

/// The reason the built-ins are not "whatever the orchestrator has": every tool
/// schema is re-sent on every step of every subagent, so a reader that inherits
/// write and browser tools pays for them on every request of a 128-agent fleet.
#[test]
fn a_built_in_reader_does_not_inherit_write_tools() {
    let empty = tempfile::tempdir().unwrap();
    let (kinds, _) = discover_agent_kinds(&[empty.path().to_path_buf()]);
    let tools = find(&kinds, "explore")
        .tools
        .clone()
        .expect("explicit list");

    assert!(
        !tools
            .iter()
            .any(|t| matches!(t.as_str(), "edit" | "write" | "multi_edit" | "bash")),
        "a read-only kind must not be able to change anything: {tools:?}"
    );
    assert!(
        !tools.iter().any(|t| t == "agent"),
        "depth 1 is structural, so no kind may hand out the agent tool: {tools:?}"
    );
}

/// A user file of a built-in's name replaces it whole. Half-overriding — taking
/// the file's brief but the built-in's tools, or the reverse — would be worse
/// than either, so the file wins outright.
#[test]
fn a_user_file_replaces_a_built_in_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "explore.md",
        "---\nname: explore\ndescription: our house rules for exploring\ntools: [read]\n---\nOur own brief.\n",
    );

    let (kinds, warnings) = discover_agent_kinds(&[dir.path().to_path_buf()]);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(
        kinds.iter().filter(|k| k.name == "explore").count(),
        1,
        "one explore, not two: {kinds:?}"
    );

    let explore = find(&kinds, "explore");
    assert_eq!(explore.description, "our house rules for exploring");
    assert_eq!(explore.prompt, "Our own brief.");
    assert_eq!(explore.tools, Some(vec!["read".to_string()]));
    assert!(
        !explore.path.to_string_lossy().starts_with("<builtin>"),
        "the file must be the one that survived"
    );
}

/// Every tool a built-in names has to exist, or a spawn silently gets fewer
/// tools than its brief promises.
#[test]
fn every_tool_a_built_in_names_is_a_real_tool() {
    let real: std::collections::HashSet<String> = crate::builtin_tools(&Default::default())
        .names()
        .map(str::to_string)
        .collect();
    let empty = tempfile::tempdir().unwrap();
    let (kinds, _) = discover_agent_kinds(&[empty.path().to_path_buf()]);

    for kind in &kinds {
        for tool in kind.tools.iter().flatten() {
            assert!(
                real.contains(tool),
                "kind {} names unknown tool {tool}; real: {real:?}",
                kind.name
            );
        }
    }
}

/// The launch histograms have to move when a spawn happens, and stay at zero
/// when nothing does. The counters are process-wide, so a test that only
/// asserted "something was recorded" would pass on a neighbour's spawn; these
/// compare against a snapshot taken immediately before the call instead.
#[test]
fn launch_counters_only_move_when_a_spawn_happens() {
    let before = pacode_types::agent_launch_stats();
    assert_eq!(
        before.resolve.count, 0,
        "nothing has spawned yet in this process"
    );

    pacode_types::record_launch_resolve(std::time::Duration::from_micros(1));
    pacode_types::record_launch_session_open(std::time::Duration::from_micros(2));
    pacode_types::record_launch_ready(std::time::Duration::from_micros(3), false);
    pacode_types::record_launch_prompt_to_first(std::time::Duration::from_micros(4));

    let after = pacode_types::agent_launch_stats();
    assert_eq!(after.spawns, before.spawns + 1);
    assert_eq!(after.refused, before.refused);
    assert_eq!(after.resolve.count, before.resolve.count + 1);
    assert_eq!(after.session_open.count, before.session_open.count + 1);
    assert_eq!(after.ready.count, before.ready.count + 1);
    assert_eq!(
        after.prompt_to_first.count,
        before.prompt_to_first.count + 1
    );
}

/// A refused spawn must be counted as refused and must *not* land in the ready
/// histogram — otherwise "slow launches" would quietly include the ones that
/// never launched, and the mean would be about the wrong population.
#[test]
fn a_refused_spawn_is_counted_separately_from_a_slow_one() {
    let before = pacode_types::agent_launch_stats();
    pacode_types::record_launch_ready(std::time::Duration::from_secs(9), true);

    let after = pacode_types::agent_launch_stats();
    assert_eq!(after.refused, before.refused + 1);
    assert_eq!(after.spawns, before.spawns, "a refusal is not a spawn");
    assert_eq!(
        after.ready.count, before.ready.count,
        "the refused call must not pollute the ready histogram"
    );
}
