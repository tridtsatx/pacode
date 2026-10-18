use pacode_types::{Effort, ModelInfo, ModelKind, ModelRoute};
use serde_json::json;

use super::*;

fn base(model: &str) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new("openrouter", model),
        display_name: model.to_string(),
        ..ModelInfo::default()
    }
}

fn fixture() -> serde_json::Value {
    json!({
        "data": [
            {
                "id": "anthropic/claude-sonnet",
                "context_length": 1000000,
                "architecture": {
                    "input_modalities": ["text", "image"],
                    "output_modalities": ["text"]
                },
                "pricing": {"prompt": "0.000003", "completion": "0.000015", "input_cache_read": "0.0000003"},
                "top_provider": {"max_completion_tokens": 64000},
                "supported_parameters": ["tools", "reasoning", "include_reasoning"]
            },
            {
                "id": "vendor/free-model:free",
                "context_length": 131072,
                "architecture": {"modality": "text->text"},
                "pricing": {"prompt": "0", "completion": "0"},
                "top_provider": {"max_completion_tokens": null},
                "supported_parameters": ["tools"]
            },
            {
                "id": "openrouter/auto",
                "pricing": {"prompt": "-1", "completion": "-1"}
            },
            {
                "id": "google/imagen-4",
                "architecture": {"output_modalities": ["image"]}
            }
        ]
    })
}

fn item(i: usize) -> serde_json::Value {
    fixture()["data"][i].clone()
}

#[test]
fn openrouter_pricing_is_scaled_to_per_million() {
    let mut info = base("anthropic/claude-sonnet");
    apply_catalog_item(&mut info, &item(0), ReasoningWire::ReasoningObject, true);
    let p = info.pricing.expect("priced");
    assert!((p.input_per_m - 3.0).abs() < 1e-9);
    assert!((p.output_per_m - 15.0).abs() < 1e-9);
    assert!((p.cache_read_per_m.unwrap_or(0.0) - 0.3).abs() < 1e-9);
    assert_eq!(info.context_window, Some(1_000_000));
    assert_eq!(info.max_output, Some(64_000));
}

#[test]
fn reasoning_flag_and_efforts_come_from_supported_parameters() {
    let mut info = base("anthropic/claude-sonnet");
    apply_catalog_item(&mut info, &item(0), ReasoningWire::ReasoningObject, true);
    assert!(info.supports_reasoning);
    assert_eq!(
        info.efforts,
        vec![Effort::Low, Effort::Medium, Effort::High]
    );

    let mut free = base("vendor/free-model:free");
    free.supports_reasoning = true; // the id heuristic guessed wrong
    apply_catalog_item(&mut free, &item(1), ReasoningWire::ReasoningObject, true);
    assert!(!free.supports_reasoning);
    assert!(free.efforts.is_empty());
}

#[test]
fn pinned_reasoning_is_not_overridden() {
    let mut free = base("vendor/free-model:free");
    free.supports_reasoning = true;
    apply_catalog_item(&mut free, &item(1), ReasoningWire::ReasoningObject, false);
    assert!(free.supports_reasoning);
    assert_eq!(free.efforts.len(), 3);
}

#[test]
fn free_models_are_zero_priced_and_negative_is_unknown() {
    let mut free = base("vendor/free-model:free");
    apply_catalog_item(&mut free, &item(1), ReasoningWire::ReasoningObject, true);
    let p = free.pricing.expect("free is priced at zero");
    assert_eq!((p.input_per_m, p.output_per_m), (0.0, 0.0));
    assert_eq!(free.max_output, None);

    let mut auto = base("openrouter/auto");
    apply_catalog_item(&mut auto, &item(2), ReasoningWire::ReasoningObject, true);
    assert!(auto.pricing.is_none());
}

#[test]
fn modalities_decide_the_kind() {
    let mut chat = base("anthropic/claude-sonnet");
    apply_catalog_item(&mut chat, &item(0), ReasoningWire::ReasoningObject, true);
    assert_eq!(chat.kind, ModelKind::Chat);
    let mut img = base("google/imagen-4");
    img.supports_reasoning = true;
    apply_catalog_item(&mut img, &item(3), ReasoningWire::ReasoningObject, true);
    assert_eq!(img.kind, ModelKind::Image);
    assert!(!img.supports_reasoning);
    assert!(img.efforts.is_empty());
}

#[test]
fn config_pricing_wins_over_the_catalog() {
    let mut info = base("anthropic/claude-sonnet");
    info.pricing = Some(Pricing {
        input_per_m: 1.0,
        output_per_m: 2.0,
        cache_read_per_m: None,
        cache_write_per_m: None,
    });
    apply_catalog_item(&mut info, &item(0), ReasoningWire::ReasoningObject, true);
    assert_eq!(info.pricing.map(|p| p.input_per_m), Some(1.0));
}

#[test]
fn efforts_follow_the_wire_convention() {
    assert!(efforts_for(ReasoningWire::ThinkingToggle, "glm-5").is_empty());
    assert!(efforts_for(ReasoningWire::Unsupported, "x").is_empty());
    assert_eq!(
        efforts_for(ReasoningWire::ReasoningEffortTiered, "kimi-k3"),
        vec![Effort::Low, Effort::High, Effort::Max]
    );
    assert_eq!(
        efforts_for(ReasoningWire::ReasoningEffort, "gemini-3-pro"),
        vec![Effort::Low, Effort::High]
    );
    assert_eq!(
        efforts_for(ReasoningWire::ReasoningEffort, "gpt-5-pro"),
        vec![Effort::High]
    );
    assert_eq!(efforts_for(ReasoningWire::ReasoningEffort, "o3").len(), 3);
}

#[test]
fn kind_from_id_patterns() {
    assert_eq!(kind_from_id("text-embedding-3-small"), ModelKind::Embedding);
    assert_eq!(kind_from_id("dall-e-3"), ModelKind::Image);
    assert_eq!(kind_from_id("whisper-1"), ModelKind::Other);
    assert_eq!(kind_from_id("gpt-5"), ModelKind::Chat);
    assert_eq!(kind_from_id("glm-4v"), ModelKind::Chat);
}
