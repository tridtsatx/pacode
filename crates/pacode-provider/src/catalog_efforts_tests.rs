use pacode_types::Effort;
use serde_json::json;

use super::efforts;

#[test]
fn the_chatgpt_backend_spelling_is_parsed() {
    // `chatgpt.com/backend-api/codex/models` answers with objects per level.
    let entry = json!({
        "slug": "gpt-5.6",
        "supported_reasoning_levels": [
            { "reasoning_effort": "minimal" },
            { "reasoning_effort": "high" },
            { "reasoning_effort": "max" }
        ]
    });
    assert_eq!(
        efforts(&entry),
        Some(vec![Effort::High, Effort::Max]),
        "minimal has no Effort counterpart and is dropped, not guessed at"
    );
}

#[test]
fn the_flat_spelling_is_parsed() {
    let entry = json!({
        "id": "gpt-5.4",
        "supported_reasoning_efforts": ["none", "low", "xhigh"]
    });
    assert_eq!(efforts(&entry), Some(vec![Effort::Low, Effort::XHigh]));
}

#[test]
fn opencodes_reasoning_options_spelling_is_parsed() {
    let entry = json!({
        "id": "deepseek-v4",
        "reasoning_options": [
            { "type": "budget" },
            { "type": "effort", "values": ["low", "high", "max"] }
        ]
    });
    assert_eq!(
        efforts(&entry),
        Some(vec![Effort::Low, Effort::High, Effort::Max])
    );
}

#[test]
fn levels_come_back_in_ladder_order_not_catalog_order() {
    let a = json!({ "supported_reasoning_efforts": ["max", "low", "high"] });
    let b = json!({ "supported_reasoning_efforts": ["low", "high", "max"] });
    assert_eq!(efforts(&a), efforts(&b));
    assert_eq!(
        efforts(&a),
        Some(vec![Effort::Low, Effort::High, Effort::Max])
    );
}

#[test]
fn a_repeated_level_is_listed_once() {
    let entry = json!({ "supported_reasoning_efforts": ["low", "low", "high"] });
    assert_eq!(efforts(&entry), Some(vec![Effort::Low, Effort::High]));
}

/// The whole point of `None`: a catalog that says nothing must not be read as
/// "no effort works", or the picker greys out a level the API accepts.
#[test]
fn a_catalog_that_says_nothing_yields_no_list() {
    assert_eq!(efforts(&json!({ "id": "gpt-4o" })), None);
    assert_eq!(
        efforts(&json!({ "id": "gpt-4o", "supported_reasoning_efforts": [] })),
        None
    );
    assert_eq!(
        efforts(&json!({ "supported_parameters": ["reasoning", "reasoning_effort"] })),
        None,
        "OpenRouter names the parameter, not the values — that is not a list"
    );
}

/// An enumeration this client cannot express is still not "nothing works": the
/// levels it did name survive, and only those.
#[test]
fn a_list_of_only_unknown_values_yields_no_list() {
    let entry = json!({ "supported_reasoning_efforts": ["none", "minimal", "swarm"] });
    assert_eq!(efforts(&entry), None);
}

/// A malformed element is skipped, not fatal: one bad row must not cost the
/// levels the rest of the catalog states.
#[test]
fn a_malformed_element_does_not_lose_the_other_levels() {
    let entry = json!({ "supported_reasoning_efforts": [null, 7, "low", { "effort": "max" }] });
    assert_eq!(efforts(&entry), Some(vec![Effort::Low, Effort::Max]));
}
