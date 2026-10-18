use super::*;
use crate::{ContentBlock, Message, Request, Role};

#[test]
fn token_estimate_is_fixed_and_capped() {
    assert_eq!(estimate_image_tokens(Some(750), Some(750)), 750);
    assert_eq!(
        estimate_image_tokens(Some(1568), Some(1568)),
        IMAGE_TOKEN_CAP
    );
    assert_eq!(estimate_image_tokens(Some(1), Some(1)), 1);
    assert_eq!(estimate_image_tokens(None, Some(10)), IMAGE_TOKEN_DEFAULT);
}

#[test]
fn format_bytes_units() {
    assert_eq!(format_bytes(12), "12 B");
    assert_eq!(format_bytes(1536), "1.5 KB");
    assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MB");
}

#[test]
fn image_block_counts_fixed_tokens_not_payload() {
    let msg = Message::new(
        Role::User,
        vec![ContentBlock::Image {
            mime: "image/png".into(),
            hash: "ab".repeat(32),
            source_path: None,
            width: Some(750),
            height: Some(750),
        }],
    );
    assert_eq!(msg.estimate_tokens(), 750);
}

#[test]
fn old_history_without_image_blocks_deserializes() {
    let old =
        r#"{"role":"user","content":[{"type":"text","text":"hi"}],"meta":{"timestamp_ms":1}}"#;
    let msg: Message = serde_json::from_str(old).unwrap();
    assert_eq!(msg.text(), "hi");
}

#[test]
fn image_block_roundtrip_omits_empty_optionals() {
    let block = ContentBlock::Image {
        mime: "image/jpeg".into(),
        hash: "cd".repeat(32),
        source_path: None,
        width: None,
        height: None,
    };
    let json = serde_json::to_value(&block).unwrap();
    assert_eq!(json["type"], "image");
    assert!(json.get("source_path").is_none());
    let back: ContentBlock = serde_json::from_value(json).unwrap();
    assert_eq!(back, block);
}

#[test]
fn user_message_without_attachments_still_parses() {
    let req: Request = serde_json::from_str(r#"{"type":"user_message","text":"hello"}"#).unwrap();
    match req {
        Request::UserMessage { text, attachments } => {
            assert_eq!(text, "hello");
            assert!(attachments.is_empty());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn attachments_roundtrip_through_protocol() {
    let req = Request::UserMessage {
        text: "look".into(),
        attachments: vec![Attachment {
            mime: "image/png".into(),
            data: "AAAA".into(),
            name: Some("shot.png".into()),
        }],
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);
}
