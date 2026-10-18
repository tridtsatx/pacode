use std::collections::BTreeMap;
use std::sync::OnceLock;

use base64::Engine as _;
use pacode_types::{CallId, ContentBlock, Message, ProviderConfig, ProviderDefaults, Role};
use serde_json::json;

use super::*;
use crate::{CompletionRequest, OpenAiCompat, Provider};

const FAKE_BYTES: &[u8] = b"fake-png-bytes";

fn store_image() -> String {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    pacode_image::store::init(dir.path().to_path_buf());
    pacode_image::store::put(FAKE_BYTES, "image/png").expect("put")
}

fn data_url_of_fake() -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(FAKE_BYTES)
    )
}

fn image_block(hash: &str) -> ContentBlock {
    ContentBlock::Image {
        mime: "image/png".into(),
        hash: hash.into(),
        source_path: Some("shot.png".into()),
        width: Some(10),
        height: Some(10),
    }
}

fn user_with_image(text: &str, hash: &str) -> Message {
    Message::new(
        Role::User,
        vec![ContentBlock::Text { text: text.into() }, image_block(hash)],
    )
}

fn req(model: &str, messages: Vec<Message>) -> CompletionRequest {
    CompletionRequest {
        model: model.to_string(),
        system_static: "sys".to_string(),
        system_dynamic: String::new(),
        messages,
        tools: vec![],
        effort: None,
        max_output_tokens: None,
    }
}

fn compat_provider() -> OpenAiCompat {
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://llm.example.test/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![],
        catalog: false,
        context_window: Some(128_000),
        reasoning: Some(false),
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
        proxy: None,
    };
    OpenAiCompat::new(
        "p",
        cfg,
        ProviderDefaults::default(),
        Some("k".to_string()),
        BTreeMap::new(),
    )
    .expect("valid provider")
}

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
fn gate_passes_vision_models_untouched() {
    let hash = store_image();
    let msgs = vec![user_with_image("look", &hash)];
    let out = gate_images(&msgs, "gpt-4o", true).expect("ok");
    assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn gate_rejects_current_images_for_text_only_models() {
    let hash = store_image();
    let msgs = vec![user_with_image("look", &hash)];
    let err = gate_images(&msgs, "plain-model", false).expect_err("must fail");
    assert!(
        err.to_string()
            .contains("model plain-model does not accept images"),
        "{err}"
    );
}

#[test]
fn gate_strips_stale_images_from_older_history() {
    let hash = store_image();
    let msgs = vec![
        user_with_image("old", &hash),
        Message::assistant_text("ok"),
        Message::user("new question"),
    ];
    let out = gate_images(&msgs, "plain-model", false).expect("ok");
    assert!(!out.iter().any(Message::has_images));
    assert!(out[0].text().contains("[image omitted: shot.png]"));
}

#[test]
fn openai_user_message_uses_content_parts() {
    let hash = store_image();
    let body = compat_provider().build_body(&req(
        "gpt-4o",
        vec![
            user_with_image("what is this?", &hash),
            Message::user("plain"),
        ],
    ));
    let content = &body["messages"][1]["content"];
    assert_eq!(content[0], json!({"type":"text","text":"what is this?"}));
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(content[1]["image_url"]["url"], data_url_of_fake());
    // Messages without images keep the plain string form.
    assert_eq!(body["messages"][2]["content"], "plain");
}

#[test]
fn openai_tool_images_follow_the_whole_tool_result_run() {
    let hash = store_image();
    let c1 = CallId::new("c1");
    let c2 = CallId::new("c2");
    let assistant = Message::new(
        Role::Assistant,
        vec![
            ContentBlock::ToolUse {
                id: c1.clone(),
                name: "read".into(),
                input: json!({}),
            },
            ContentBlock::ToolUse {
                id: c2.clone(),
                name: "read".into(),
                input: json!({}),
            },
        ],
    );
    let r1 = Message::tool_result_with_images(
        c1,
        "image file",
        false,
        &[pacode_types::ImageRef {
            hash,
            mime: "image/png".into(),
            width: Some(10),
            height: Some(10),
            name: Some("a.png".into()),
        }],
    );
    let r2 = Message::tool_result(c2, "text", false);
    let body = compat_provider().build_body(&req("gpt-4o", vec![assistant, r1, r2]));
    let msgs = body["messages"].as_array().expect("array");
    // system, assistant, tool, tool, user follow-up
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[3]["role"], "tool");
    assert_eq!(msgs[4]["role"], "user");
    assert_eq!(
        msgs[4]["content"][1]["image_url"]["url"],
        data_url_of_fake()
    );
}

#[test]
fn missing_image_bytes_degrade_to_a_text_note() {
    store_image();
    let gone = "e".repeat(64);
    let body = compat_provider().build_body(&req("gpt-4o", vec![user_with_image("hi", &gone)]));
    assert_eq!(
        body["messages"][1]["content"][1],
        json!({"type":"text","text":"[image omitted: shot.png]"})
    );
}

#[tokio::test]
async fn complete_fails_before_sending_for_text_only_models() {
    let hash = store_image();
    let provider = compat_provider();
    let result = provider
        .complete(req("plain-model", vec![user_with_image("look", &hash)]))
        .await;
    match result {
        Err(err) => assert!(
            err.to_string()
                .contains("model plain-model does not accept images"),
            "{err}"
        ),
        Ok(_) => panic!("expected a pre-send error"),
    }
}

#[test]
fn model_info_reports_vision() {
    let p = compat_provider();
    assert!(p.model_info("gpt-4o").vision);
    assert!(!p.model_info("plain-model").vision);
}

#[test]
fn codex_user_message_uses_input_image() {
    let hash = store_image();
    let cfg = ProviderConfig {
        kind: Default::default(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: None,
        api_key_env: None,
        models: vec![],
        catalog: false,
        context_window: Some(128_000),
        reasoning: Some(false),
        effort_map: BTreeMap::new(),
        extra_body: None,
        headers: BTreeMap::new(),
        proxy: None,
    };
    let provider = crate::codex::Codex::new(
        "codex",
        cfg,
        ProviderDefaults::default(),
        None,
        BTreeMap::new(),
    )
    .expect("valid provider");
    let body = provider.build_body(&req("gpt-5", vec![user_with_image("see", &hash)]));
    let content = &body["input"][0]["content"];
    assert_eq!(content[0], json!({"type":"input_text","text":"see"}));
    assert_eq!(
        content[1],
        json!({"type":"input_image","image_url": data_url_of_fake()})
    );
}

#[test]
fn devin_user_message_has_nested_field_10_and_placeholder() {
    let hash = store_image();
    let encoded = crate::devin::wire::encode_messages(&[user_with_image("see", &hash)]);
    assert_eq!(encoded.len(), 1);
    let bytes = encoded.into_iter().next().expect("one").into_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("[Image 1: shot.png]"), "{text}");
    let b64 = base64::engine::general_purpose::STANDARD.encode(FAKE_BYTES);
    assert!(text.contains(&b64));
    assert!(text.contains("image/png"));
}

#[test]
fn redact_data_urls_drops_payloads() {
    let s = r#"{"url":"data:image/png;base64,AAAABBBB=="},"x":1}"#;
    let out = redact_data_urls(s);
    assert!(out.contains("<10 bytes>"), "{out}");
    assert!(!out.contains("AAAA"));
    assert!(out.ends_with(r#""},"x":1}"#), "{out}");
}
