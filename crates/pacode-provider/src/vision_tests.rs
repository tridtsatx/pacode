use serde_json::json;

use super::*;

#[test]
fn heuristic_marks_known_vision_families() {
    for yes in [
        "gpt-4o",
        "gpt-4.1-mini",
        "gpt-5.1-codex",
        "o3",
        "o4-mini",
        "gemini-2.5-flash",
        "models/gemini-3-pro",
        "moonshotai/kimi-k2.5",
        "kimi-k3",
        "moonshot-v1-8k-vision-preview",
        "glm-4.5v",
        "glm-4.1v-thinking-flash",
        "qwen2.5-vl-72b",
        "llava:13b",
        "openai/gpt-4o-mini",
    ] {
        assert!(heuristic_vision(yes), "{yes} should accept images");
    }
    for no in [
        "gpt-oss-120b",
        "o3-mini",
        "o1-mini",
        "glm-4.6",
        "glm-5",
        "kimi-k2-0905-preview",
        "deepseek-chat",
        "llama3.1:8b",
        "unknown-model",
    ] {
        assert!(!heuristic_vision(no), "{no} should not accept images");
    }
}

#[test]
fn catalog_declares_vision() {
    let with = json!({"id":"a","architecture":{"input_modalities":["text","image"]}});
    let without = json!({"id":"b","architecture":{"input_modalities":["text"]}});
    let legacy = json!({"id":"c","architecture":{"modality":"text+image->text"}});
    let none = json!({"id":"d"});
    assert_eq!(catalog_vision(&with), Some(true));
    assert_eq!(catalog_vision(&without), Some(false));
    assert_eq!(catalog_vision(&legacy), Some(true));
    assert_eq!(catalog_vision(&none), None);
}

#[test]
fn redact_data_urls_drops_payloads() {
    let s = r#"{"url":"data:image/png;base64,AAAABBBB=="},"x":1}"#;
    let out = redact_data_urls(s);
    assert!(out.contains("<10 bytes>"), "{out}");
    assert!(!out.contains("AAAA"));
    assert!(out.ends_with(r#""},"x":1}"#), "{out}");
}
