use super::*;
use pacode_types::Mode;

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

    // 1. Neither exists -> None
    assert!(load_memory(&proj, Some(&home), 8000).is_none());

    // 2. Only global exists
    std::fs::write(home.join("memory.md"), "- global note 1\n- global note 2\n").unwrap();
    let loaded_global = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(loaded_global.starts_with("# Memory (global)"));
    assert!(loaded_global.contains("- global note 1"));
    assert!(loaded_global.contains("- global note 2"));
    assert!(!loaded_global.contains("# Memory (project)"));

    // 3. Only project exists
    std::fs::remove_file(home.join("memory.md")).unwrap();
    let proj_pacode = proj.join(".pacode");
    std::fs::create_dir_all(&proj_pacode).unwrap();
    std::fs::write(proj_pacode.join("memory.md"), "- proj note 1\n").unwrap();
    let loaded_proj = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(!loaded_proj.contains("# Memory (global)"));
    assert!(loaded_proj.starts_with("# Memory (project)"));
    assert!(loaded_proj.contains("- proj note 1"));
}

#[test]
fn test_load_memory_capping_keeps_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home_config");
    let proj = tmp.path().join("proj");
    let proj_pacode = proj.join(".pacode");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj_pacode).unwrap();

    // Global has older notes; project has newer notes
    std::fs::write(
        home.join("memory.md"),
        "- global line 1 (oldest)\n- global line 2\n- global line 3\n",
    )
    .unwrap();
    std::fs::write(
        proj_pacode.join("memory.md"),
        "- proj line 1\n- proj line 2 (newest)\n",
    )
    .unwrap();

    let full = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(full.contains("global line 1"));
    assert!(full.contains("proj line 2"));

    // Cap to length that drops oldest global line(s) and includes visible marker
    let capped = load_memory(&proj, Some(&home), 90).unwrap();
    assert!(capped.chars().count() <= 90);
    // Newest line must be kept
    assert!(capped.contains("proj line 2 (newest)"));
    // Oldest global line must be dropped
    assert!(!capped.contains("global line 1 (oldest)"));
    assert!(capped.contains("characters truncated"));

    // Even tighter cap drops all global lines and keeps only project tail
    let tight = load_memory(&proj, Some(&home), 50).unwrap();
    assert!(tight.chars().count() <= 50);
    assert!(!tight.contains("# Memory (global)"));
    assert!(tight.contains("proj line 2 (newest)"));
}

#[test]
fn test_configured_memory_cap_truncates_with_marker_on_char_boundary() {
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

    // With large cap (8000), no truncation occurs
    let untruncated = load_memory(&proj, Some(&home), 8000).unwrap();
    assert!(!untruncated.contains("characters truncated"));

    // With tight configured cap (120 chars), truncation must happen
    let cap = 120;
    let truncated = load_memory(&proj, Some(&home), cap).unwrap();

    // 1. Output length is strictly within configured cap
    assert!(
        truncated.chars().count() <= cap,
        "length {} must be <= cap {}",
        truncated.chars().count(),
        cap
    );

    // 2. Visible marker is present and consistent with truncate_head_tail
    assert!(
        truncated.contains("[... ") && truncated.contains("characters truncated ...]"),
        "expected visible marker in: {truncated}"
    );

    // 3. Newest project notes are kept
    assert!(
        truncated.contains("🎯"),
        "expected newest project content to be preserved"
    );

    // 4. Multi-byte string slicing didn't panic and produced valid UTF-8
    assert!(!truncated.is_empty());
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
    };
    let out_empty = system_dynamic(&ctx_empty);
    assert!(!out_empty.contains("## Skills"));
}
