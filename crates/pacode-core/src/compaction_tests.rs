use std::sync::Arc;

use pacode_types::{CallId, ContentBlock, Message, ModelInfo, ModelRoute, Role};

use super::*;

#[test]
fn test_needs_compaction_basic() {
    assert!(!needs_compaction(500, 1000, 0.8));
    assert!(!needs_compaction(799, 1000, 0.8));
    assert!(needs_compaction(800, 1000, 0.8));
    assert!(needs_compaction(1200, 1000, 0.8));
    assert!(!needs_compaction(1000, 0, 0.8));
}

#[test]
fn test_compaction_triggers_at_right_fraction_for_small_window_model() {
    // Model with 8K context window (e.g. older/local model), threshold 0.85
    let small_window = 8_000;
    let threshold = 0.85;
    let cutoff = (small_window as f32 * threshold) as u32; // 6,800

    // Just below cutoff: no compaction
    assert!(
        !needs_compaction(cutoff - 1, small_window, threshold),
        "should not compact at 6799/8000"
    );

    // Exactly at cutoff: compacts
    assert!(
        needs_compaction(cutoff, small_window, threshold),
        "should compact at 6800/8000"
    );

    // Above cutoff: compacts
    assert!(
        needs_compaction(7_500, small_window, threshold),
        "should compact at 7500/8000"
    );
}

#[test]
fn test_compaction_triggers_at_right_fraction_for_large_window_model() {
    // Model with 200K context window (e.g. Claude 3.5 / Gemini 1.5), threshold 0.85
    let large_window = 200_000;
    let threshold = 0.85;
    let cutoff = (large_window as f32 * threshold) as u32; // 170,000

    // Just below cutoff: no compaction
    assert!(
        !needs_compaction(cutoff - 1, large_window, threshold),
        "should not compact at 169999/200000"
    );

    // Exactly at cutoff: compacts
    assert!(
        needs_compaction(cutoff, large_window, threshold),
        "should compact at 170000/200000"
    );

    // Above cutoff: compacts
    assert!(
        needs_compaction(190_000, large_window, threshold),
        "should compact at 190000/200000"
    );
}

#[test]
fn test_compaction_fallback_when_no_advertised_window() {
    // 1. ModelInfo advertises a real window -> uses advertised window
    let model_with_window = ModelInfo {
        route: ModelRoute::new("provider", "small-model"),
        display_name: "Small Model".to_string(),
        context_window: Some(16_000),
        supports_reasoning: false,
        vision: false,
        pricing: None,
        ..Default::default()
    };
    let resolved = resolve_context_window(Some(&model_with_window), 128_000);
    assert_eq!(resolved, 16_000);

    // 2. ModelInfo reports context_window: None -> uses config default
    let model_without_window = ModelInfo {
        route: ModelRoute::new("provider", "custom-model"),
        display_name: "Custom Model".to_string(),
        context_window: None,
        supports_reasoning: false,
        vision: false,
        pricing: None,
        ..Default::default()
    };
    let resolved_config = resolve_context_window(Some(&model_without_window), 64_000);
    assert_eq!(resolved_config, 64_000);

    // 3. ModelInfo reports context_window: None and config default is 0 -> documented fallback 128,000
    let resolved_fallback = resolve_context_window(Some(&model_without_window), 0);
    assert_eq!(resolved_fallback, DEFAULT_FALLBACK_CONTEXT_WINDOW);
    assert_eq!(resolved_fallback, 128_000);

    // 4. No ModelInfo at all -> uses config default
    let resolved_none = resolve_context_window(None, 32_000);
    assert_eq!(resolved_none, 32_000);
}

#[test]
fn test_no_compaction_when_usage_below_threshold() {
    let window = 128_000;
    let threshold = 0.85;
    let trigger_point = (window as f32 * threshold) as u32; // 108,800

    // Low usage (start of session)
    assert!(!needs_compaction(1_000, window, threshold));
    assert!(!needs_compaction(20_000, window, threshold));
    assert!(!needs_compaction(50_000, window, threshold));
    assert!(!needs_compaction(100_000, window, threshold));
    assert!(!needs_compaction(trigger_point - 1, window, threshold));

    // Reaching and exceeding threshold
    assert!(needs_compaction(trigger_point, window, threshold));
    assert!(needs_compaction(trigger_point + 5_000, window, threshold));
}

#[test]
fn test_split_for_compaction_basic() {
    let msgs: Vec<Arc<Message>> = (0..10)
        .map(|i| Arc::new(Message::user(format!("msg {i}"))))
        .collect();

    let (to_summarize, to_keep) = split_for_compaction(&msgs, 4);
    assert_eq!(to_summarize.len(), 6);
    assert_eq!(to_keep.len(), 4);
    assert_eq!(to_summarize[0].text(), "msg 0");
    assert_eq!(to_keep[0].text(), "msg 6");
}

#[test]
fn test_split_for_compaction_keeps_tool_pairs() {
    let call_id = CallId::generate();
    let msgs = vec![
        Arc::new(Message::user("do something")),
        Arc::new(Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: call_id.clone(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }],
        )),
        Arc::new(Message::tool_result(call_id, "file1.txt", false)),
        Arc::new(Message::user("next prompt")),
    ];

    let (to_summarize, to_keep) = split_for_compaction(&msgs, 2);
    assert_eq!(to_summarize.len(), 1);
    assert_eq!(to_summarize[0].text(), "do something");
    assert_eq!(to_keep.len(), 3);
    assert_eq!(to_keep[0].role, Role::Assistant);
    assert_eq!(to_keep[1].role, Role::Tool);
}

#[test]
fn test_split_for_compaction_keep_zero_does_not_panic() {
    let msgs: Vec<Arc<Message>> = (0..3)
        .map(|i| Arc::new(Message::user(format!("msg {i}"))))
        .collect();
    let (to_summarize, to_keep) = split_for_compaction(&msgs, 0);
    assert_eq!(to_summarize.len(), 3);
    assert!(to_keep.is_empty());
}

#[test]
fn test_context_config_defaults_for_pipeline() {
    let ctx = pacode_types::ContextConfig::default();
    assert!((ctx.compaction_threshold - 0.85).abs() < 1e-6);
    assert!((ctx.compaction_target_ratio - 0.40).abs() < 1e-6);
    assert_eq!(ctx.keep_recent_turns, 6);
    assert_eq!(ctx.keep_recent_messages, 6);
}

struct NoPins;
impl CompactionPinned for NoPins {}

struct TablePins;
impl CompactionPinned for TablePins {
    fn pinned_blocks(&self) -> Vec<PinnedBlock> {
        vec!["## File ownership\nsrc/a.rs: agent-1".to_string()]
    }
}

#[test]
fn test_pinned_hook_defaults_to_empty() {
    assert!(NoPins.pinned_blocks().is_empty());
    assert_eq!(TablePins.pinned_blocks().len(), 1);
}

#[test]
fn test_attach_and_split_pinned_round_trip() {
    let blocks = vec![
        "## A\n1".to_string(),
        "  ".to_string(),
        "## B\n2".to_string(),
    ];
    let full = pinned::attach_pinned("body text\n", &blocks);
    let (body, pins) = split_pinned(&full);
    assert_eq!(body, "body text");
    assert_eq!(pins, Some("## A\n1\n\n## B\n2"));
    assert_eq!(pinned::attach_pinned("body", &[]), "body");
    assert_eq!(split_pinned("plain"), ("plain", None));
}

#[test]
fn test_plan_block_lists_items_verbatim() {
    use pacode_types::{Plan, PlanItem, PlanStatus};
    assert!(pinned::plan_block(&Plan::default()).is_none());
    let plan = Plan {
        version: 3,
        items: vec![
            PlanItem {
                id: "1".into(),
                content: "write tests".into(),
                status: PlanStatus::Done,
                progress: None,
            },
            PlanItem {
                id: "2".into(),
                content: "fix `foo::bar`".into(),
                status: PlanStatus::Active,
                progress: Some(40),
            },
        ],
    };
    let block = pinned::plan_block(&plan).expect("block");
    assert!(block.contains("[x] 1 (done): write tests"));
    assert!(block.contains("[~] 2 (active): fix `foo::bar` [40%]"));
}

#[test]
fn test_tasks_block_empty_when_no_background_tasks() {
    assert!(pinned::tasks_block(&[]).is_none());
}

#[test]
fn test_instruction_pointer_block_lists_existing_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        pinned::instruction_pointer_block(dir.path())
            .map(|b| !b.contains(&dir.path().display().to_string()))
            .unwrap_or(true)
    );
    std::fs::write(dir.path().join("AGENTS.md"), "rules").expect("write");
    let block = pinned::instruction_pointer_block(dir.path()).expect("block");
    assert!(block.contains(&dir.path().join("AGENTS.md").display().to_string()));
}

#[test]
fn test_digest_lists_files_and_commands_deduplicated() {
    let mut msgs: Vec<Arc<Message>> = Vec::new();
    for (name, input, err) in [
        ("edit", serde_json::json!({"path": "src/a.rs"}), false),
        ("edit", serde_json::json!({"path": "src/a.rs"}), false),
        ("read", serde_json::json!({"path": "src/b.rs"}), false),
        ("bash", serde_json::json!({"command": "cargo test"}), true),
    ] {
        let id = CallId::generate();
        msgs.push(Arc::new(Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: name.into(),
                input,
            }],
        )));
        msgs.push(Arc::new(Message::tool_result(id, "out", err)));
    }
    let d = pinned::digest(&msgs);
    assert_eq!(d.matches("edit src/a.rs").count(), 1);
    assert!(d.contains("read src/b.rs"));
    assert!(d.contains("`cargo test` -> failed"));
}
