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
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    };
    let resolved = resolve_context_window(Some(&model_with_window), 128_000);
    assert_eq!(resolved, 16_000);

    // 2. ModelInfo reports context_window: None -> uses config default
    let model_without_window = ModelInfo {
        route: ModelRoute::new("provider", "custom-model"),
        display_name: "Custom Model".to_string(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
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

// ---------------------------------------------------------------------------
// The split must not fold away the request being carried out, nor the record of
// what a mutating call actually did.
// ---------------------------------------------------------------------------

mod protected {
    use super::*;

    /// `user` / `assistant text` / `tool_use(name)` / `tool_result` runs.
    fn tool_use(name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: CallId::generate(),
            name: name.to_string(),
            input: serde_json::json!({}),
        }
    }

    fn result_of(call: &ContentBlock) -> Arc<Message> {
        let ContentBlock::ToolUse { id, .. } = call else {
            panic!("expected a ToolUse block");
        };
        Arc::new(Message::tool_result(id.clone(), "ok", false))
    }

    /// The body of a `tool_result` block. `Message::text` concatenates only
    /// `Text` blocks, so a tool result reads as the empty string through it —
    /// asserting on `text()` for one of these would pass whatever the code did.
    fn tool_result_text(message: &Message) -> String {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rows_of(messages: &[Arc<Message>]) -> Vec<(u64, Arc<Message>)> {
        messages
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, message)| (index as u64, message))
            .collect()
    }

    #[test]
    fn a_distant_request_is_carried_by_the_summary_not_by_the_cut() {
        // The request is four messages back, well outside a `keep_recent` of 2.
        // It is handed to the summariser instead of pinning the cut, so the
        // agent still learns what it was asked to do while the burst folds.
        let call = tool_use("read");
        let msgs = vec![
            Arc::new(Message::user("rename the config parser")),
            Arc::new(Message::new(Role::Assistant, vec![call.clone()])),
            result_of(&call),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: "looking".into(),
                }],
            )),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: "still looking".into(),
                }],
            )),
        ];

        let rows = rows_of(&msgs);
        let split = split_rows_for_compaction(&rows, 2);

        let request = split
            .active_request
            .expect("the request in flight must be recovered from the folded rows");
        assert_eq!(request.text(), "rename the config parser");

        // It is pinned into the prompt the summariser answers, so a summary that
        // omits it is a summary that is wrong about the task.
        let prompt = build_summary_request(
            None,
            &split
                .to_summarize
                .iter()
                .map(|(_, m)| m.clone())
                .collect::<Vec<_>>(),
            Some(&request),
        );
        let rendered = prompt
            .iter()
            .map(|m| m.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("rename the config parser"),
            "the request must appear in the summary prompt"
        );
    }

    #[test]
    fn no_request_is_pinned_when_the_newest_turn_is_already_kept() {
        // Nothing was folded past the request, so there is nothing to carry —
        // pinning a request the agent can already see would be noise.
        let msgs = vec![
            Arc::new(Message::user("do the thing")),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: "working".into(),
                }],
            )),
        ];
        let rows = rows_of(&msgs);
        let split = split_rows_for_compaction(&rows, 2);
        assert!(split.to_summarize.is_empty());
        assert!(split.active_request.is_none());
    }

    #[test]
    fn the_summary_prompt_omits_the_request_block_when_there_is_none() {
        let msgs = vec![
            Arc::new(Message::user("old")),
            Arc::new(Message::user("newer")),
        ];
        let prompt = build_summary_request(None, &msgs, None);
        let rendered = prompt
            .iter()
            .map(|m| m.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rendered.contains("Request currently being executed"),
            "no request was folded, so no request block belongs in the prompt"
        );
    }

    #[test]
    fn a_mutating_result_near_the_cut_moves_the_cut_down() {
        // The cheap case: the record of the mutation is within the lookback, so
        // keeping it costs one message and no summary-prompt ceremony.
        let call = tool_use("edit");
        let msgs = vec![
            Arc::new(Message::user("go")),
            Arc::new(Message::new(Role::Assistant, vec![call.clone()])),
            result_of(&call),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "a".into() }],
            )),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "b".into() }],
            )),
        ];
        let (to_summarize, to_keep) = split_for_compaction(&msgs, 2);
        assert!(
            to_keep.iter().any(|m| m.role == Role::Tool),
            "the result of an `edit` is the record of the workspace and must survive"
        );
        assert!(
            to_summarize.len() < 3,
            "the cut moved down to keep it, folded {}",
            to_summarize.len()
        );
    }

    #[test]
    fn a_failed_mutating_call_survives_too() {
        // The retry after a failed edit depends on knowing what the edit
        // actually did, which is only in the result.
        let call = tool_use("write");
        let msgs = vec![
            Arc::new(Message::new(Role::Assistant, vec![call.clone()])),
            {
                let ContentBlock::ToolUse { id, .. } = &call else {
                    panic!("expected a ToolUse block");
                };
                Arc::new(Message::tool_result(id.clone(), "permission denied", true))
            },
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "a".into() }],
            )),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "b".into() }],
            )),
        ];

        let (_, to_keep) = split_for_compaction(&msgs, 2);
        assert!(
            to_keep
                .iter()
                .any(|m| tool_result_text(m).contains("permission denied")),
            "the failure that explains the current state must survive, kept: {:?}",
            to_keep
                .iter()
                .map(|m| (m.role, tool_result_text(m)))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn read_only_results_are_not_protected() {
        // A `read` result is reproducible by re-reading; protecting every tool
        // result would keep most of a long session and stop compaction working.
        let call = tool_use("read");
        let msgs = vec![
            Arc::new(Message::new(Role::Assistant, vec![call.clone()])),
            result_of(&call),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "a".into() }],
            )),
            Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "b".into() }],
            )),
        ];

        let (to_summarize, to_keep) = split_for_compaction(&msgs, 2);
        assert_eq!(to_keep.len(), 2, "no reason to keep more than asked");
        assert!(
            to_summarize.iter().any(|m| m.role == Role::Tool),
            "the read result is folded"
        );
    }

    #[test]
    fn older_user_messages_are_folded_not_carried() {
        // Only the *newest* request is carried. Carrying every user turn would
        // mean a conversation of nothing but user messages accumulates requests
        // in every successive summary.
        let msgs: Vec<Arc<Message>> = (0..10)
            .map(|i| Arc::new(Message::user(format!("msg {i}"))))
            .collect();

        let (to_summarize, to_keep) = split_for_compaction(&msgs, 4);
        assert_eq!(to_keep.len(), 4, "the tail is exactly keep_recent");
        assert_eq!(to_keep[0].text(), "msg 6");
        assert!(!to_summarize.is_empty());
    }

    #[test]
    fn a_distant_request_does_not_stop_the_session_compacting() {
        // The request is 40 messages back. Honouring it by moving the cut would
        // pin all forty, and a session that compacts repeatedly would re-grow
        // without ever shrinking — the failure compaction exists to cure. So the
        // cut is untouched and the request is carried in the summary prompt
        // instead: the agent still learns the task, and the tail still shrinks.
        let mut msgs = vec![Arc::new(Message::user("the original request"))];
        for i in 0..40 {
            msgs.push(Arc::new(Message::new(
                Role::Assistant,
                vec![ContentBlock::Text {
                    text: format!("step {i}"),
                }],
            )));
        }

        let (to_summarize, to_keep) = split_for_compaction(&msgs, 2);
        assert_eq!(
            to_keep.len(),
            2,
            "a distant request must not pin the messages behind it, kept {}",
            to_keep.len()
        );
        assert_eq!(to_summarize.len(), 39, "everything but the tail folds");

        // The guarantee the cut gave up is made up for by the summary prompt.
        let rows = rows_of(&msgs);
        let split = split_rows_for_compaction(&rows, 2);
        let request = split.active_request.expect("the request is recovered");
        assert_eq!(request.text(), "the original request");
        assert_eq!(
            split.to_keep.len(),
            2,
            "and it is not kept as a message either — it travels in the prompt"
        );
    }

    #[test]
    fn mutating_results_far_behind_the_cut_are_folded() {
        // The bounded tier. A hundred old `edit` results must not each veto
        // compaction; the bound exists so the tail cannot grow with the number
        // of past mutations.
        let mut msgs = Vec::new();
        for _ in 0..40 {
            let call = tool_use("edit");
            msgs.push(Arc::new(Message::new(Role::Assistant, vec![call.clone()])));
            msgs.push(result_of(&call));
        }

        let (to_summarize, to_keep) = split_for_compaction(&msgs, 4);
        let kept_results = to_keep.iter().filter(|m| m.role == Role::Tool).count();
        assert!(
            kept_results <= MAX_EXTRA_KEPT_MESSAGES,
            "old mutating results must not be kept without bound, kept {kept_results}"
        );
        // Compaction has to actually reclaim something. The kept tail is
        // `keep_recent` plus one pulled-back `tool_result` for the pairing
        // guard, and at most `MAX_EXTRA_KEPT_MESSAGES` of rescue — anything
        // close to "all of it" is the unbounded-growth failure this bound
        // exists to prevent, whatever the exact cut lands on.
        assert!(
            to_summarize.len() >= 80 - 2 * MAX_EXTRA_KEPT_MESSAGES,
            "compaction must still fold the bulk of the history, folded {} of 80",
            to_summarize.len()
        );
    }

    #[test]
    fn a_read_only_session_keeps_exactly_the_requested_tail() {
        // Nothing is protected, so the cut must not move at all. This is the
        // guard against the lookback window silently becoming a default: a
        // read-only session with no protected message must keep `keep_recent`
        // and nothing more.
        let mut msgs = Vec::new();
        for _ in 0..20 {
            let call = tool_use("read");
            msgs.push(Arc::new(Message::new(Role::Assistant, vec![call.clone()])));
            msgs.push(result_of(&call));
        }

        let (_, to_keep) = split_for_compaction(&msgs, 4);
        assert_eq!(
            to_keep.len(),
            4,
            "with no protected message below the cut it must not move, kept {}",
            to_keep.len()
        );
    }

    #[test]
    fn the_split_stays_a_valid_tool_pairing_boundary() {
        // Protection must not reintroduce the orphan-result the existing
        // pairing guard exists to prevent.
        let call = tool_use("edit");
        let msgs = vec![
            Arc::new(Message::user("go")),
            Arc::new(Message::new(Role::Assistant, vec![call.clone()])),
            result_of(&call),
        ];

        let (to_summarize, to_keep) = split_for_compaction(&msgs, 1);
        // Whatever the split is, the first kept message may not be a bare
        // `tool_result`.
        assert!(
            to_keep.first().map(|m| m.role) != Some(Role::Tool),
            "kept tail must not start with an orphaned tool_result"
        );
        // And every kept tool result must have its tool_use kept too.
        let mut kept_ids: Vec<CallId> = Vec::new();
        for msg in &to_keep {
            for (id, _, _) in msg.tool_uses() {
                kept_ids.push(id.clone());
            }
        }
        for msg in &to_keep {
            for block in &msg.content {
                if let ContentBlock::ToolResult { call_id, .. } = block {
                    assert!(
                        kept_ids.contains(call_id),
                        "kept a tool_result whose tool_use was folded away"
                    );
                }
            }
        }
        let _ = to_summarize;
    }
}

// ---------------------------------------------------------------------------
// ZDR: the summary request carries the whole conversation prefix, so the route
// that produces it has to be inside the retention boundary.
// ---------------------------------------------------------------------------

mod zdr {
    use super::*;

    fn route(provider: &str, model: &str) -> ModelRoute {
        ModelRoute::new(provider, model)
    }

    fn capable(keys: &[&str]) -> impl Fn(&str) -> bool {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        move |key: &str| keys.iter().any(|k| k == key)
    }

    #[test]
    fn without_zdr_the_configured_model_wins() {
        let agent_route = route("mock", "agent-model");
        let (picked, refusal) = pick_summary_route(
            Some(route("other", "cheap")),
            &agent_route,
            false,
            &capable(&[]),
        );
        assert_eq!(picked, route("other", "cheap"));
        assert!(refusal.is_none());
    }

    #[test]
    fn without_zdr_a_route_nobody_flagged_is_still_used() {
        let (picked, refusal) =
            pick_summary_route(None, &route("mock", "agent-model"), false, &capable(&[]));
        assert_eq!(picked, route("mock", "agent-model"));
        assert!(refusal.is_none());
    }

    #[test]
    fn under_zdr_a_flagged_compaction_model_is_used() {
        let (picked, refusal) = pick_summary_route(
            Some(route("other", "cheap")),
            &route("mock", "agent-model"),
            true,
            &capable(&["mock/agent-model", "other/cheap"]),
        );
        assert_eq!(picked, route("other", "cheap"));
        assert!(refusal.is_none());
    }

    #[test]
    fn under_zdr_an_unflagged_compaction_model_falls_back_to_the_agent_route() {
        let (picked, refusal) = pick_summary_route(
            Some(route("other", "cheap")),
            &route("mock", "agent-model"),
            true,
            &capable(&["mock/agent-model"]),
        );
        assert_eq!(
            picked,
            route("mock", "agent-model"),
            "the summary must not go to a route outside the ZDR boundary"
        );
        assert!(refusal.is_none());
    }

    #[test]
    fn under_zdr_an_unflagged_agent_route_refuses_instead_of_leaking() {
        let (picked, refusal) =
            pick_summary_route(None, &route("mock", "agent-model"), true, &capable(&[]));
        assert_eq!(picked, route("mock", "agent-model"));
        let reason = refusal.expect("compaction must be refused, not leaked");
        assert!(
            reason.starts_with("zdr_blocked:"),
            "reason must be recognisable as policy: {reason}"
        );
    }

    #[test]
    fn under_zdr_no_route_at_all_is_usable_refuses() {
        let (picked, refusal) = pick_summary_route(
            Some(route("other", "cheap")),
            &route("mock", "agent-model"),
            true,
            &capable(&[]),
        );
        assert_eq!(picked, route("mock", "agent-model"));
        assert!(refusal.is_some());
    }
}
