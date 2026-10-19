use pacode_types::{
    CallId, ContentBlock, Message, Role, TokenCategory, ToolDefinition, Usage, estimate_tokens,
    estimate_tokens_for_chars,
};
use serde_json::json;

use super::RequestAttribution;

fn tool(name: &str, description_chars: usize) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: "d".repeat(description_chars),
        input_schema: json!({"type": "object"}),
    }
}

fn assistant_call(name: &str) -> Message {
    Message::new(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: CallId::new("call_1"),
            name: name.to_string(),
            input: json!({"pattern": "needle"}),
        }],
    )
}

fn tool_result(content: &str) -> Message {
    Message::new(
        Role::Tool,
        vec![ContentBlock::ToolResult {
            call_id: CallId::new("call_1"),
            content: content.to_string(),
            is_error: false,
        }],
    )
}

#[test]
fn a_huge_tool_result_is_charged_to_results_and_not_to_history() {
    // The finding the page exists for: one grep that flooded the context. It
    // must not disappear into the `history` row, which would leave the user
    // staring at a large-but-unremarkable conversation number.
    let flood = "match\n".repeat(40_000);
    let history: Vec<std::sync::Arc<Message>> = vec![
        std::sync::Arc::new(Message::user("find the needle")),
        std::sync::Arc::new(assistant_call("grep")),
        std::sync::Arc::new(tool_result(&flood)),
    ];

    let attribution = RequestAttribution::from_request("sys", "", &[], &history);

    assert_eq!(
        attribution.categories.tool_results,
        u64::from(estimate_tokens(&flood)),
    );
    // History is only the user line and the tool call itself.
    let conversation_chars = history[0].char_len() + history[1].char_len();
    assert_eq!(
        attribution.categories.history,
        u64::from(estimate_tokens_for_chars(conversation_chars)),
    );
    assert!(
        attribution.categories.tool_results > attribution.categories.history * 10,
        "the flood should dominate, got {:?}",
        attribution.categories
    );
}

#[test]
fn the_split_sums_to_the_total_it_was_derived_from() {
    let static_prompt = "you are pacode".repeat(10);
    let dynamic_prompt = "cwd: /tmp\nbranch: main".to_string();
    let tools = vec![tool("grep", 4_000), tool("read", 1_000)];
    let history: Vec<std::sync::Arc<Message>> = vec![
        std::sync::Arc::new(Message::user("hello")),
        std::sync::Arc::new(assistant_call("read")),
        std::sync::Arc::new(tool_result("file body")),
    ];
    let usage = Usage {
        input_tokens: 4_321,
        output_tokens: 120,
        reasoning_tokens: 30,
        cache_read_tokens: 900,
        cache_write_tokens: 0,
    };

    let attribution =
        RequestAttribution::from_request(&static_prompt, &dynamic_prompt, &tools, &history)
            .with_usage(&usage);

    assert_eq!(
        attribution.categories.instructions,
        u64::from(estimate_tokens(&static_prompt)) + u64::from(estimate_tokens(&dynamic_prompt))
    );
    assert_eq!(
        attribution.categories.tool_defs,
        u64::from(tools[0].estimate_tokens()) + u64::from(tools[1].estimate_tokens())
    );
    assert_eq!(attribution.categories.reply, 150, "output plus reasoning");
    // The five rows add up to the total the table prints, and each row is the
    // estimator applied to the text it claims to cover.
    let hand_sum = attribution.categories.instructions
        + attribution.categories.tool_defs
        + attribution.categories.history
        + attribution.categories.tool_results
        + attribution.categories.reply;
    assert_eq!(attribution.categories.total(), hand_sum);
    for (category, tokens) in attribution.categories.rows() {
        assert_eq!(attribution.categories.get(category), tokens);
    }
    // The provider's own numbers pass through untouched — they are the only
    // real counts in here and must not be re-estimated.
    assert_eq!(attribution.reported_input, 4_321);
    assert_eq!(attribution.reported_output, 120);
    assert_eq!(attribution.cache_read, 900);
}

#[test]
fn a_mixed_message_does_not_lose_its_prose_to_the_results_row() {
    // A provider that merges a result and the next user turn into one
    // message: the text belongs to history, only the result block does not.
    let merged = Message::new(
        Role::Tool,
        vec![
            ContentBlock::ToolResult {
                call_id: CallId::new("call_1"),
                content: "x".repeat(400),
                is_error: false,
            },
            ContentBlock::Text {
                text: "y".repeat(400),
            },
        ],
    );

    let attribution = RequestAttribution::from_request("", "", &[], &[std::sync::Arc::new(merged)]);

    assert_eq!(attribution.categories.tool_results, 100);
    assert_eq!(attribution.categories.history, 100);
    assert_eq!(attribution.categories.get(TokenCategory::ToolResults), 100);
}
