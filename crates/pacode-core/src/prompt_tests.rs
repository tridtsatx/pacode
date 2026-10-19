use super::*;
use pacode_types::Mode;
use std::sync::Arc;

#[test]
fn test_load_instructions_order() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let parent = tmp.path().join("parent");
    let child = parent.join("child");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&child).unwrap();

    std::fs::write(home.join("AGENTS.md"), "global agents").unwrap();
    std::fs::write(home.join("PACODE.md"), "global pacode").unwrap();

    std::fs::write(parent.join("AGENTS.md"), "parent agents").unwrap();
    std::fs::write(parent.join("PACODE.md"), "parent pacode").unwrap();
    std::fs::write(parent.join("CLAUDE.md"), "parent claude").unwrap();

    std::fs::write(child.join("AGENTS.md"), "child agents").unwrap();
    std::fs::write(child.join("PACODE.md"), "child pacode").unwrap();
    std::fs::write(child.join("CLAUDE.md"), "child claude").unwrap();

    let loaded = load_instructions(&child, Some(&home), 100_000).unwrap();

    let pos_global_agents = loaded.find("global agents").unwrap();
    let pos_global_pacode = loaded.find("global pacode").unwrap();
    let pos_parent_agents = loaded.find("parent agents").unwrap();
    let pos_parent_pacode = loaded.find("parent pacode").unwrap();
    let pos_parent_claude = loaded.find("parent claude").unwrap();
    let pos_child_agents = loaded.find("child agents").unwrap();
    let pos_child_pacode = loaded.find("child pacode").unwrap();
    let pos_child_claude = loaded.find("child claude").unwrap();

    assert!(pos_global_agents < pos_global_pacode);
    assert!(pos_global_pacode < pos_parent_agents);
    assert!(pos_parent_agents < pos_parent_pacode);
    assert!(pos_parent_pacode < pos_parent_claude);
    assert!(pos_parent_claude < pos_child_agents);
    assert!(pos_child_agents < pos_child_pacode);
    assert!(pos_child_pacode < pos_child_claude);
}

#[test]
fn test_load_memory_skipping_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    // 1. Neither exists -> None, and nothing is created on disk
    assert!(load_memory(&proj, Some(&home), 8000).is_none());
    assert!(!home.join("memory.db").exists());
    assert!(!proj.join(".pacode").exists());

    // 2. Only global exists (legacy memory.md is imported as pinned entries)
    std::fs::write(home.join("memory.md"), "- global note 1\n- global note 2\n").unwrap();
    let loaded_global = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(loaded_global.starts_with("# Memory (global)"));
    assert!(loaded_global.contains("- global note 1"));
    assert!(loaded_global.contains("- global note 2"));
    assert!(loaded_global.contains("memory_recall"));
    assert!(!loaded_global.contains("# Memory (project)"));
    assert!(home.join("memory.md.migrated").exists());

    // 3. Only project exists
    let home2 = tmp.path().join("home_config2");
    let proj2 = tmp.path().join("proj2");
    std::fs::create_dir_all(&home2).unwrap();
    std::fs::create_dir_all(proj2.join(".pacode")).unwrap();
    std::fs::write(proj2.join(".pacode").join("memory.md"), "- proj note 1\n").unwrap();
    let loaded_proj = load_memory(&proj2, Some(&home2), 8000).unwrap();
    assert!(!loaded_proj.contains("# Memory (global)"));
    assert!(loaded_proj.starts_with("# Memory (project)"));
    assert!(loaded_proj.contains("- proj note 1"));
}

#[test]
fn test_load_memory_cap_drops_index_lines_first() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(proj.join(".pacode")).unwrap();

    // 25 bullets: the first 20 are pinned, the last 5 become index lines.
    let md: String = (0..25).map(|i| format!("- global line {i}\n")).collect();
    std::fs::write(home.join("memory.md"), md).unwrap();

    let full = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(full.contains("global line 0 ("));
    assert!(full.contains("Recent (index only)"));
    assert!(full.contains("global line 24"));
    assert!(!full.contains("more memories omitted"));

    // Same store, same text: byte-identical (prompt-cache friendly).
    assert_eq!(full, load_memory(&proj, Some(&home), 8000).unwrap());

    let capped = load_memory(&proj, Some(&home), 700).unwrap();
    assert!(capped.chars().count() <= 700);
    assert!(capped.contains("more memories omitted"));
    assert!(!capped.contains("Recent (index only)"));
    assert!(capped.contains("global line 0 ("));

    let tight = load_memory(&proj, Some(&home), 50).unwrap();
    assert!(tight.chars().count() <= 50);
    assert!(load_memory(&proj, Some(&home), 0).is_none());
}

#[test]
fn test_configured_memory_cap_is_char_boundary_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let proj = tmp.path().join("proj");
    let proj_pacode = proj.join(".pacode");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj_pacode).unwrap();

    // Multi-byte characters (Cyrillic, emoji, accents) to verify character boundary safety
    let global_text = "- Привет мир: старая глобальная запись ✨ 🦀 12345\n".repeat(10);
    let proj_text = "- Проектная заметка: свежие данные важные для работы 🎯\n";
    std::fs::write(home.join("memory.md"), &global_text).unwrap();
    std::fs::write(proj_pacode.join("memory.md"), proj_text).unwrap();

    let untruncated = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(!untruncated.contains("more memories omitted"));
    assert!(untruncated.contains("🎯"));

    for cap in [30usize, 120, 400] {
        let truncated = load_memory(&proj, Some(&home), cap).unwrap();
        assert!(
            truncated.chars().count() <= cap,
            "length {} must be <= cap {cap}",
            truncated.chars().count()
        );
        assert!(!truncated.is_empty());
    }
}

#[test]
fn test_load_instructions_appends_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let proj = tmp.path().join("proj");
    let proj_pacode = proj.join(".pacode");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj_pacode).unwrap();

    std::fs::write(proj.join("AGENTS.md"), "instruction content").unwrap();
    std::fs::write(home.join("memory.md"), "- remember global").unwrap();
    std::fs::write(proj_pacode.join("memory.md"), "- remember project").unwrap();

    let loaded = load_instructions(&proj, Some(&home), 10_000).unwrap();
    assert!(loaded.contains("instruction content"));
    assert!(loaded.contains("# Memory (global)"));
    assert!(loaded.contains("- remember global"));
    assert!(loaded.contains("# Memory (project)"));
    assert!(loaded.contains("- remember project"));

    let pos_instr = loaded.find("instruction content").unwrap();
    let pos_global = loaded.find("# Memory (global)").unwrap();
    let pos_proj = loaded.find("# Memory (project)").unwrap();

    assert!(pos_instr < pos_global);
    assert!(pos_global < pos_proj);
}

#[test]
fn test_prompt_prefix_byte_identical_across_steps_of_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();

    // Create initial git HEAD and instructions
    let git_dir = proj.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(proj.join("AGENTS.md"), "Initial instructions v1").unwrap();

    // Step 1: Turn start initializes TurnPromptCache
    let turn_cache = TurnPromptCache::new(&proj, Some(&home), 32_000, 8_000);
    let plan = pacode_types::Plan::default();

    let dyn_ctx_step1 = DynamicContext {
        cwd: &proj,
        git_branch: turn_cache.git_branch.as_deref(),
        date: &turn_cache.date,
        mode: Mode::Build,
        plan: &plan,
        instructions: turn_cache.instructions.as_deref(),
        is_subagent: false,
        skills: &[],
        skills_enabled: false,
        max_listed_skills: 10,
        tools: &[],
        provider: native_provider(),
    };
    let step1_prompt = system_dynamic(&dyn_ctx_step1);
    assert!(step1_prompt.contains("Git branch: main"));
    assert!(step1_prompt.contains("Initial instructions v1"));

    // Mid-turn mutation: a tool or external process modifies .git/HEAD and AGENTS.md
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature-branch\n").unwrap();
    std::fs::write(proj.join("AGENTS.md"), "Modified instructions v2").unwrap();

    // Step 2: During the same turn, reusing turn_cache
    let dyn_ctx_step2 = DynamicContext {
        cwd: &proj,
        git_branch: turn_cache.git_branch.as_deref(),
        date: &turn_cache.date,
        mode: Mode::Build,
        plan: &plan,
        instructions: turn_cache.instructions.as_deref(),
        is_subagent: false,
        skills: &[],
        skills_enabled: false,
        max_listed_skills: 10,
        tools: &[],
        provider: native_provider(),
    };
    let step2_prompt = system_dynamic(&dyn_ctx_step2);

    // PROOF: Prompt prefix is byte-identical across two steps of one turn!
    assert_eq!(
        step1_prompt.as_bytes(),
        step2_prompt.as_bytes(),
        "prompt prefix must be byte-identical across steps of the same turn"
    );

    // Next turn start: cache invalidation point recomputes fresh values
    let next_turn_cache = TurnPromptCache::new(&proj, Some(&home), 32_000, 8_000);
    let dyn_ctx_next_turn = DynamicContext {
        cwd: &proj,
        git_branch: next_turn_cache.git_branch.as_deref(),
        date: &next_turn_cache.date,
        mode: Mode::Build,
        plan: &plan,
        instructions: next_turn_cache.instructions.as_deref(),
        is_subagent: false,
        skills: &[],
        skills_enabled: false,
        max_listed_skills: 10,
        tools: &[],
        provider: native_provider(),
    };
    let next_turn_prompt = system_dynamic(&dyn_ctx_next_turn);
    assert!(next_turn_prompt.contains("Git branch: feature-branch"));
    assert!(next_turn_prompt.contains("Modified instructions v2"));
    assert_ne!(step1_prompt.as_bytes(), next_turn_prompt.as_bytes());
}

#[test]
fn test_system_dynamic_skills_section() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();

    let long_desc = "x".repeat(300);
    let skills = vec![
        pacode_skills::Skill {
            name: "skill-a".to_string(),
            description: "Description for skill A".to_string(),
            dir: cwd.join("skills/skill-a"),
            path: cwd.join("skills/skill-a/SKILL.md"),
        },
        pacode_skills::Skill {
            name: "skill-b".to_string(),
            description: long_desc,
            dir: cwd.join("skills/skill-b"),
            path: cwd.join("skills/skill-b/SKILL.md"),
        },
        pacode_skills::Skill {
            name: "skill-c".to_string(),
            description: "Skill C should be omitted if max_listed is 2".to_string(),
            dir: cwd.join("skills/skill-c"),
            path: cwd.join("skills/skill-c/SKILL.md"),
        },
    ];

    // 1. Skills enabled and present
    let ctx = DynamicContext {
        cwd: &cwd,
        git_branch: None,
        date: "2026-09-09",
        mode: Mode::Build,
        plan: &plan,
        instructions: None,
        is_subagent: false,
        skills: &skills,
        skills_enabled: true,
        max_listed_skills: 2,
        tools: &[],
        provider: native_provider(),
    };
    let out = system_dynamic(&ctx);
    assert!(out.contains("## Skills\n"));
    assert!(out.contains("- skill-a: Description for skill A\n"));
    // skill-b description must be capped at 200 chars
    assert!(out.contains(&format!("- skill-b: {}\n", "x".repeat(200))));
    assert!(!out.contains(&"x".repeat(201)));
    // skill-c should not be present because max_listed_skills is 2
    assert!(!out.contains("- skill-c"));

    // 2. Skills disabled -> emits NOTHING about skills
    let ctx_disabled = DynamicContext {
        cwd: &cwd,
        git_branch: None,
        date: "2026-09-09",
        mode: Mode::Build,
        plan: &plan,
        instructions: None,
        is_subagent: false,
        skills: &skills,
        skills_enabled: false,
        max_listed_skills: 10,
        tools: &[],
        provider: native_provider(),
    };
    let out_disabled = system_dynamic(&ctx_disabled);
    assert!(!out_disabled.contains("## Skills"));
    assert!(!out_disabled.contains("skill-a"));

    // 3. Skills empty -> emits NOTHING about skills
    let ctx_empty = DynamicContext {
        cwd: &cwd,
        git_branch: None,
        date: "2026-09-09",
        mode: Mode::Build,
        plan: &plan,
        instructions: None,
        is_subagent: false,
        skills: &[],
        skills_enabled: true,
        max_listed_skills: 10,
        tools: &[],
        provider: native_provider(),
    };
    let out_empty = system_dynamic(&ctx_empty);
    assert!(!out_empty.contains("## Skills"));
}

fn shared_opts(home: &Path) -> PromptOpts {
    PromptOpts {
        home_config: Some(home.to_path_buf()),
        instructions_cap_chars: 32_000,
        memory_cap_chars: 0,
        discover_foreign: false,
    }
}

#[tokio::test]
async fn shared_cache_is_one_arc_until_main_turn_or_ttl() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("AGENTS.md"), "v1").unwrap();
    let opts = shared_opts(&tmp.path().join("home"));
    let cache = shared::SharedPromptCache::new();
    let t0 = Instant::now();

    let a = cache.resolve(&proj, opts.clone(), false, t0).await;
    let b = cache.resolve(&proj, opts.clone(), false, t0).await;
    assert!(Arc::ptr_eq(&a, &b), "two subagents share one cache");

    // Edits stay invisible until a main turn start...
    std::fs::write(proj.join("AGENTS.md"), "v2").unwrap();
    let c = cache.resolve(&proj, opts.clone(), false, t0).await;
    assert!(Arc::ptr_eq(&a, &c));
    // ...which invalidates and rebuilds.
    let m = cache.resolve(&proj, opts.clone(), true, t0).await;
    assert!(!Arc::ptr_eq(&a, &m));
    assert!(m.instructions.as_deref().unwrap_or("").contains("v2"));
    let d = cache.resolve(&proj, opts.clone(), false, t0).await;
    assert!(Arc::ptr_eq(&m, &d));

    // TTL: refreshed after 5 s.
    std::fs::write(proj.join("AGENTS.md"), "v3").unwrap();
    let late = t0 + shared::TTL + Duration::from_millis(1);
    let e = cache.resolve(&proj, opts, false, late).await;
    assert!(!Arc::ptr_eq(&d, &e));
    assert!(e.instructions.as_deref().unwrap_or("").contains("v3"));
}

#[tokio::test]
async fn shared_cache_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = shared_opts(&tmp.path().join("home"));
    let cache = shared::SharedPromptCache::new();
    let now = std::time::Instant::now();
    let mut first = None;
    for i in 0..(shared::MAX_CWDS + 4) {
        let dir = tmp.path().join(format!("p{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let v = cache.resolve(&dir, opts.clone(), false, now).await;
        if i == 0 {
            first = Some((dir, v));
        }
    }
    let (dir, v) = first.unwrap();
    let again = cache.resolve(&dir, opts, false, now).await;
    assert!(!Arc::ptr_eq(&v, &again), "oldest entry was evicted");
}

#[test]
fn subagent_dynamic_prompt_ignores_plan_and_mode() {
    use pacode_types::{Plan, PlanItem, PlanStatus};
    let cwd = std::path::PathBuf::from("/tmp/x");
    let empty = Plan::default();
    let mut planned = Plan {
        version: 2,
        ..Plan::default()
    };
    planned.items.push(PlanItem {
        id: "p1".into(),
        content: "do the thing".into(),
        status: PlanStatus::Active,
        progress: None,
    });
    let build = |plan: &Plan, mode: Mode, sub: bool| {
        system_dynamic(&DynamicContext {
            cwd: &cwd,
            git_branch: None,
            date: "2026-01-01",
            mode,
            plan,
            instructions: None,
            is_subagent: sub,
            skills: &[],
            skills_enabled: false,
            max_listed_skills: 0,
            tools: &[],
            provider: native_provider(),
        })
    };
    assert_eq!(
        build(&empty, Mode::Build, true),
        build(&planned, Mode::Plan, true)
    );
    assert!(build(&planned, Mode::Build, false).contains("do the thing"));
}

fn tools_ctx<'a>(
    cwd: &'a std::path::Path,
    plan: &'a pacode_types::Plan,
    tools: &'a [ToolDefinition],
    is_subagent: bool,
) -> DynamicContext<'a> {
    DynamicContext {
        cwd,
        git_branch: Some("main"),
        date: "2026-10-13",
        mode: Mode::Build,
        plan,
        instructions: None,
        is_subagent,
        skills: &[],
        skills_enabled: false,
        max_listed_skills: 10,
        tools,
        provider: native_provider(),
    }
}

/// A provider that sends native tool names on the wire.
fn native_provider() -> &'static pacode_provider::mock::MockProvider {
    static NATIVE: std::sync::LazyLock<pacode_provider::mock::MockProvider> =
        std::sync::LazyLock::new(|| pacode_provider::mock::MockProvider::new("mock"));
    &NATIVE
}

fn tool(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }
}

fn builtin_definitions() -> Vec<ToolDefinition> {
    pacode_tools::builtin::builtin_tools(&pacode_types::WebConfig::default()).definitions()
}

fn tools_section_of(out: &str) -> &str {
    out.split("## Tools\n").nth(1).expect("tools section")
}

#[test]
fn tools_section_lists_every_builtin_with_a_one_line_summary() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let defs = builtin_definitions();
    assert!(
        defs.iter().any(|d| d.name == "agent"),
        "builtins carry `agent`"
    );
    let out = system_dynamic(&tools_ctx(&cwd, &plan, &defs, false));
    let section = tools_section_of(&out);
    for def in &defs {
        let line = section
            .lines()
            .find(|l| l.starts_with(&format!("- {}: ", def.name)))
            .unwrap_or_else(|| panic!("{} listed", def.name));
        assert!(
            line.chars().count() <= def.name.chars().count() + 4 + 120,
            "{line}"
        );
    }
    assert_eq!(section.lines().count(), defs.len());
    assert!(!section.contains("more not listed"));
}

#[test]
fn subagent_tools_section_matches_its_reduced_set() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let defs: Vec<_> = builtin_definitions()
        .into_iter()
        .filter(|d| d.name != "agent")
        .collect();
    let out = system_dynamic(&tools_ctx(&cwd, &plan, &defs, true));
    let section = tools_section_of(&out);
    assert!(!section.contains("- agent: "));
    assert!(section.contains("- read: "));
}

/// The role text names the channel tools only when the subagent has them: a
/// narrowed tool set must not be told to call a tool it was not given.
#[test]
fn subagent_role_mentions_only_the_channel_tools_it_has() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let role_of = |out: &str| -> String {
        out.split("## Subagent Role\n")
            .nth(1)
            .and_then(|rest| rest.split("\n\n").next())
            .expect("a role section")
            .to_string()
    };

    let full = vec![tool("ask_parent", "Ask."), tool("report_status", "Report.")];
    let role = role_of(&system_dynamic(&tools_ctx(&cwd, &plan, &full, true)));
    assert!(role.contains("`ask_parent`"), "{role}");
    assert!(role.contains("`report_status`"), "{role}");
    assert!(role.contains("<orchestrator_message>"), "{role}");

    let narrow = vec![tool("read", "Read.")];
    let role = role_of(&system_dynamic(&tools_ctx(&cwd, &plan, &narrow, true)));
    assert!(!role.contains("ask_parent"), "{role}");
    assert!(!role.contains("report_status"), "{role}");
    assert!(role.contains("<orchestrator_message>"), "{role}");
}

#[test]
fn tools_section_lists_mcp_and_summarises_the_first_sentence() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let mut defs = builtin_definitions();
    defs.push(tool(
        "github__search_issues",
        "Search issues in a repository. Supports qualifiers.\nSecond line.",
    ));
    defs.push(tool("long", &"y".repeat(300)));
    defs.push(tool(
        "wrapped",
        "List files (e.g. `*.rs`) matching\n    a pattern. More.\n\nNext paragraph.",
    ));
    let out = system_dynamic(&tools_ctx(&cwd, &plan, &defs, false));
    assert!(out.contains("- wrapped: List files (e.g. `*.rs`) matching a pattern.\n"));
    assert!(out.contains("- github__search_issues: Search issues in a repository.\n"));
    assert!(out.contains(&format!("- long: {}...\n", "y".repeat(117))));
}

#[test]
fn a_long_summary_is_cut_at_a_word_boundary() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let defs = vec![tool("words", &"word, ".repeat(40))];
    let out = system_dynamic(&tools_ctx(&cwd, &plan, &defs, false));
    let line = out
        .lines()
        .find(|l| l.starts_with("- words: "))
        .expect("words line");
    let summary = &line["- words: ".len()..];
    assert!(summary.ends_with("word..."), "{summary}");
    assert!(summary.chars().count() <= 120, "{summary}");
}

#[test]
fn tools_section_is_capped_with_a_marker() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let many: Vec<_> = (0..MAX_LISTED_TOOLS + 5)
        .map(|i| tool(&format!("t{i:03}"), "Short."))
        .collect();
    let out = system_dynamic(&tools_ctx(&cwd, &plan, &many, false));
    assert!(out.contains(&format!("- t{:03}: ", MAX_LISTED_TOOLS - 1)));
    assert!(!out.contains(&format!("- t{MAX_LISTED_TOOLS:03}: ")));
    assert!(out.contains("- ... and 5 more not listed here"));

    let wide: Vec<_> = (0..60)
        .map(|i| tool(&format!("w{i:02}"), &"z".repeat(300)))
        .collect();
    let out = system_dynamic(&tools_ctx(&cwd, &plan, &wide, false));
    let section = tools_section_of(&out);
    let listed = section.lines().filter(|l| l.starts_with("- w")).count();
    assert!(listed < 60);
    assert!(section.contains(&format!("- ... and {} more not listed here", 60 - listed)));
    assert!(section.chars().count() <= MAX_TOOLS_SECTION_CHARS + 200);
}

#[test]
fn prompt_is_byte_stable_while_the_tool_set_is_unchanged() {
    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let first = builtin_definitions();
    let second = builtin_definitions();
    let a = system_dynamic(&tools_ctx(&cwd, &plan, &first, false));
    let b = system_dynamic(&tools_ctx(&cwd, &plan, &second, false));
    assert_eq!(a.as_bytes(), b.as_bytes());
    // The rule lives in the static part, which never sees the tool list.
    assert!(!system_static().contains("- read: "));
}

#[test]
fn static_prompt_has_the_self_knowledge_rule() {
    let s = system_static();
    assert!(s.contains("## Self-Knowledge"));
    assert!(s.contains("never read files, search the repository, or inspect pacode's source"));
    assert!(s.contains("A tool that is not listed is one you do not have"));
}

#[test]
fn tools_section_uses_the_names_the_provider_sends_on_the_wire() {
    use pacode_provider::Provider;

    let cwd = std::path::PathBuf::from("/test/cwd");
    let plan = pacode_types::Plan::default();
    let defs = builtin_definitions();

    let native = system_dynamic(&tools_ctx(&cwd, &plan, &defs, false));
    let native = tools_section_of(&native);
    assert!(native.contains("- bash: "), "{native}");
    assert!(native.contains("- read: "), "{native}");
    assert!(!native.contains("- Bash: "), "{native}");

    let oauth = pacode_provider::anthropic::Anthropic::from_oauth(
        "anthropic",
        pacode_types::ProviderConfig {
            base_url: "https://api.anthropic.com/v1".to_string(),
            ..Default::default()
        },
        pacode_types::ProviderDefaults::default(),
        "sk-ant-oat-test".to_string(),
        Default::default(),
    )
    .expect("oauth provider");
    let mut ctx = tools_ctx(&cwd, &plan, &defs, false);
    ctx.provider = &oauth;
    let out = system_dynamic(&ctx);
    let section = tools_section_of(&out);
    for def in &defs {
        let wire = oauth.wire_tool_name(&def.name);
        assert!(
            section.contains(&format!("- {wire}: ")),
            "{wire} listed:\n{section}"
        );
    }
    assert!(section.contains("- Bash: "), "{section}");
    assert!(section.contains("- Read: "), "{section}");
    assert!(!section.contains("- bash: "), "{section}");
}
