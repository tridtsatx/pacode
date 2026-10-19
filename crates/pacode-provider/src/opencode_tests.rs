use pacode_types::Effort;
use serde_json::json;

use super::*;

fn sample_catalog() -> Value {
    json!({
        "opencode": {
            "id": "opencode",
            "api": "https://opencode.ai/zen/v1",
            "npm": "@ai-sdk/openai-compatible",
            "env": ["OPENCODE_API_KEY"],
            "models": {
                "deepseek-v4-flash-free": {
                    "id": "deepseek-v4-flash-free",
                    "name": "DeepSeek V4 Flash Free",
                    "reasoning": true,
                    "reasoning_options": [
                        {"type": "effort", "values": ["low", "high", "max"]}
                    ],
                    "tool_call": true,
                    "attachment": false,
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "limit": {"context": 200000, "output": 128000},
                    "cost": {"input": 0, "output": 0, "cache_read": 0}
                },
                "muse-spark-1.3-contributor-free": {
                    "id": "muse-spark-1.3-contributor-free",
                    "name": "Muse Spark 1.3 Free",
                    "reasoning": true,
                    "reasoning_options": [
                        {"type": "effort", "values": ["low", "medium", "high"]}
                    ],
                    "tool_call": true,
                    "attachment": true,
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "limit": {"context": 1048576, "output": 131072},
                    "cost": {"input": 0, "output": 0, "cache_read": 0}
                },
                "claude-sonnet-4-20250514": {
                    "id": "claude-sonnet-4-20250514",
                    "name": "Claude Sonnet 4",
                    "reasoning": true,
                    "reasoning_options": [
                        {"type": "effort", "values": ["low", "medium", "high", "max"]}
                    ],
                    "tool_call": true,
                    "attachment": true,
                    "modalities": {"input": ["text", "image", "pdf"], "output": ["text"]},
                    "limit": {"context": 200000, "output": 64000},
                    "cost": {"input": 3, "output": 15, "cache_read": 0.3}
                },
                "glm-5-free": {
                    "id": "glm-5-free",
                    "name": "GLM-5 Free",
                    "reasoning": true,
                    "reasoning_options": [{"type": "toggle"}],
                    "tool_call": true,
                    "attachment": false,
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "limit": {"context": 204800, "output": 131072},
                    "cost": {"input": 0, "output": 0, "cache_read": 0}
                }
            }
        }
    })
}

#[test]
fn filters_only_free_models() {
    let catalog = sample_catalog();
    let models = parse_free_models(&catalog).unwrap();
    // 3 free, 1 paid (claude-sonnet-4) should be filtered out
    assert_eq!(models.len(), 3);
    assert!(models.iter().all(|m| m.route.provider == "opencode"));
    assert!(
        models
            .iter()
            .all(|m| m.pricing.as_ref().unwrap().input_per_m == 0.0)
    );
    // Paid model not present
    assert!(
        !models
            .iter()
            .any(|m| m.route.model == "claude-sonnet-4-20250514")
    );
}

#[test]
fn maps_model_fields_correctly() {
    let catalog = sample_catalog();
    let models = parse_free_models(&catalog).unwrap();

    let ds = models
        .iter()
        .find(|m| m.route.model == "deepseek-v4-flash-free")
        .unwrap();
    assert_eq!(ds.display_name, "DeepSeek V4 Flash Free");
    assert_eq!(ds.context_window, Some(200_000));
    assert!(ds.supports_reasoning);
    assert!(!ds.supports_vision);
    assert_eq!(
        ds.supported_efforts,
        Some(vec![Effort::Low, Effort::High, Effort::Max])
    );
}

#[test]
fn maps_vision_from_attachment() {
    let catalog = sample_catalog();
    let models = parse_free_models(&catalog).unwrap();

    let muse = models
        .iter()
        .find(|m| m.route.model == "muse-spark-1.3-contributor-free")
        .unwrap();
    assert!(muse.supports_vision);
}

#[test]
fn toggle_only_reasoning_gives_no_efforts() {
    let catalog = sample_catalog();
    let models = parse_free_models(&catalog).unwrap();

    let glm = models
        .iter()
        .find(|m| m.route.model == "glm-5-free")
        .unwrap();
    assert!(glm.supports_reasoning);
    assert_eq!(glm.supported_efforts, None);
}

#[test]
fn missing_opencode_provider_is_error() {
    let catalog = json!({"other": {}});
    let result = parse_free_models(&catalog);
    assert!(result.is_err());
}

#[test]
fn opencode_headers_are_valid() {
    let headers = opencode_headers("test-session-123");
    assert_eq!(headers.len(), 4);

    let ua = headers.iter().find(|(k, _)| k == "User-Agent").unwrap();
    assert!(ua.1.starts_with("opencode/"));

    let client = headers
        .iter()
        .find(|(k, _)| k == "x-opencode-client")
        .unwrap();
    assert_eq!(client.1, "cli");

    let session = headers
        .iter()
        .find(|(k, _)| k == "x-opencode-session")
        .unwrap();
    assert_eq!(session.1, "test-session-123");

    let req_id = headers
        .iter()
        .find(|(k, _)| k == "x-opencode-request")
        .unwrap();
    assert_eq!(req_id.1.len(), 32); // 16 bytes = 32 hex chars
}

#[test]
fn random_hex_id_is_unique() {
    let a = random_hex_id();
    let b = random_hex_id();
    assert_ne!(a, b);
    assert_eq!(a.len(), 32);
}
