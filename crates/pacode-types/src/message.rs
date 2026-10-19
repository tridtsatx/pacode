//! Model-facing conversation history. This is the canonical form kept by the daemon
//! and persisted to SQLite; provider adapters translate it to their wire format and
//! the daemon translates it to [`crate::transcript::TranscriptItem`] for clients.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::CallId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Tool results. One message per result, immediately after the assistant
    /// message that requested it (OpenAI shape; other adapters merge as needed).
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Model reasoning. Kept in history only when the provider requires it to be
    /// replayed; never shown to the model as plain text.
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: CallId,
        name: String,
        input: Value,
    },
    ToolResult {
        call_id: CallId,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// An image attached to a message. Provider adapters encode it when the
    /// model supports vision; otherwise it degrades to a text placeholder
    /// ([`ImageSource::placeholder`]). In a `Role::Tool` message an `Image`
    /// block belongs to the tool result that precedes it.
    Image {
        source: ImageSource,
    },
}

/// Where the bytes of a [`ContentBlock::Image`] live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSource {
    /// Path on disk (temp paste file, `@`-ref target, …).
    Path(std::path::PathBuf),
    /// Raw bytes for providers that want inline data.
    Bytes { mime: String, data: Vec<u8> },
    /// Only metadata: the bytes are stored once, content-addressed by `hash`,
    /// in the image store (`pacode-image`) and loaded lazily by whoever needs
    /// pixels.
    Stored {
        /// `image/png` or `image/jpeg` (prepared images) and friends.
        mime: String,
        /// Lowercase hex SHA-256 of the stored bytes.
        hash: String,
        /// Where the image came from (file path), for placeholders and stubs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
}

/// Chars an image block counts toward `Message::char_len`. A vision model
/// charges roughly 1500 tokens per image regardless of file size; counting
/// serialized bytes (or zero for `Path`) would wreck the window estimate.
const IMAGE_ESTIMATE_CHARS: usize = 6_000;

impl ContentBlock {
    /// Block for a stored image.
    pub fn image(image: &crate::ImageRef) -> Self {
        ContentBlock::Image {
            source: ImageSource::stored(image),
        }
    }
}

impl ImageSource {
    /// Source for an image kept in the content-addressed image store.
    pub fn stored(image: &crate::ImageRef) -> Self {
        ImageSource::Stored {
            mime: image.mime.clone(),
            hash: image.hash.clone(),
            source_path: image.name.clone(),
            width: image.width,
            height: image.height,
        }
    }

    /// The store reference when this is a [`ImageSource::Stored`] image.
    pub fn image_ref(&self) -> Option<crate::ImageRef> {
        match self {
            ImageSource::Stored {
                mime,
                hash,
                source_path,
                width,
                height,
            } => Some(crate::ImageRef {
                hash: hash.clone(),
                mime: mime.clone(),
                width: *width,
                height: *height,
                name: source_path.clone(),
            }),
            ImageSource::Path(_) | ImageSource::Bytes { .. } => None,
        }
    }

    /// Chars this image counts toward `Message::char_len`: a fixed estimate
    /// (4 chars per token), never the payload size.
    pub fn estimate_chars(&self) -> usize {
        match self {
            ImageSource::Stored { width, height, .. } => {
                crate::estimate_image_tokens(*width, *height) as usize * 4
            }
            ImageSource::Path(_) | ImageSource::Bytes { .. } => IMAGE_ESTIMATE_CHARS,
        }
    }

    /// Text stand-in for surfaces that cannot show images (transcript, search
    /// index, non-vision degrade). Mirrors the `[image file: …]` shape that
    /// `expand.rs` emits for `@`-referenced images.
    pub fn placeholder(&self) -> String {
        match self {
            ImageSource::Path(path) => format!("[image file: {}]", path.display()),
            ImageSource::Bytes { mime, data } => {
                format!("[image: {mime}, {} bytes]", data.len())
            }
            ImageSource::Stored {
                mime, source_path, ..
            } => match source_path {
                Some(path) => format!("[image file: {path}]"),
                None => format!("[image: {mime}]"),
            },
        }
    }
}

/// Why a message exists. Drives UI visibility and compaction rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Normal,
    /// Injected between steps: task finished, agent finished, user steer.
    Injected,
    /// Summary produced by compaction; replaces older history.
    CompactionSummary,
    /// Hidden system reminder appended to a user message.
    SystemReminder,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageMeta {
    pub timestamp_ms: u64,
    pub kind: MessageKind,
    /// Hidden from the transcript UI (still sent to the model).
    pub hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub meta: MessageMeta,
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self {
            role,
            content,
            meta: MessageMeta {
                timestamp_ms: crate::time::now_ms(),
                ..MessageMeta::default()
            },
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentBlock::Text { text: text.into() }])
    }

    /// User message with `text` first (when non-empty) and the images in order.
    pub fn user_with_images(text: impl Into<String>, images: &[crate::ImageRef]) -> Self {
        let text = text.into();
        let mut content = Vec::with_capacity(1 + images.len());
        if !text.is_empty() || images.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        content.extend(images.iter().map(ContentBlock::image));
        Self::new(Role::User, content)
    }

    /// Tool result followed by the images it produced.
    pub fn tool_result_with_images(
        call_id: CallId,
        content: impl Into<String>,
        is_error: bool,
        images: &[crate::ImageRef],
    ) -> Self {
        let mut msg = Self::tool_result(call_id, content, is_error);
        msg.content.extend(images.iter().map(ContentBlock::image));
        msg
    }

    /// The images of this message in order.
    pub fn image_refs(&self) -> Vec<crate::ImageRef> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Image { source } => source.image_ref(),
                ContentBlock::Text { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::ToolUse { .. }
                | ContentBlock::ToolResult { .. } => None,
            })
            .collect()
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::new(
            Role::Assistant,
            vec![ContentBlock::Text { text: text.into() }],
        )
    }

    pub fn tool_result(call_id: CallId, content: impl Into<String>, is_error: bool) -> Self {
        Self::new(
            Role::Tool,
            vec![ContentBlock::ToolResult {
                call_id,
                content: content.into(),
                is_error,
            }],
        )
    }

    pub fn with_kind(mut self, kind: MessageKind) -> Self {
        self.meta.kind = kind;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.meta.hidden = true;
        self
    }

    /// Concatenated text blocks (reasoning excluded).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = (&CallId, &str, &Value)> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some((id, name.as_str(), input)),
            ContentBlock::Text { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Image { .. } => None,
        })
    }

    /// Whether any block is an image.
    pub fn has_images(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. }))
    }

    pub fn has_tool_uses(&self) -> bool {
        self.tool_uses().next().is_some()
    }

    /// Characters that will be sent to the model.
    pub fn char_len(&self) -> usize {
        self.content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => text.chars().count(),
                ContentBlock::Reasoning { text, .. } => text.chars().count(),
                ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
                ContentBlock::ToolResult { content, .. } => content.chars().count(),
                ContentBlock::Image { source } => source.estimate_chars(),
            })
            .sum()
    }

    /// chars/4 heuristic, the same everywhere in pacode.
    pub fn estimate_tokens(&self) -> u32 {
        estimate_tokens_for_chars(self.char_len())
    }
}

/// Tool definition advertised to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolDefinition {
    pub fn estimate_tokens(&self) -> u32 {
        let chars = self.name.len() + self.description.len() + self.input_schema.to_string().len();
        estimate_tokens_for_chars(chars)
    }
}

/// The chars/4 token heuristic used for budgets when the provider has not
/// reported real usage yet.
pub fn estimate_tokens(text: &str) -> u32 {
    estimate_tokens_for_chars(text.chars().count())
}

pub fn estimate_tokens_for_chars(chars: usize) -> u32 {
    chars.div_ceil(4) as u32
}

/// Size limit for a tool call's transcript preview — the body the TUI cell can
/// show and expand. Display-only: the model-facing `content` has its own cap.
pub const PREVIEW_CAP_CHARS: usize = 16_000;

/// Smallest cap that leaves room for the marker line. Below it the head+tail cut
/// runs without one: the marker would not fit, and a marker that eats a quarter
/// of the budget is worse than a silent cut at that size.
const TRUNCATION_MARKER_MIN_CAP: usize = 64;

/// Cut `text` to `cap` characters of payload, keeping head and tail.
///
/// The result is `cap` characters of useful text **plus** the marker line
/// (`"... N characters truncated ..."`), so a caller budgeting on the result has
/// to allow `cap + marker` — the marker is never subtracted from the payload,
/// because callers cap the *payload* they asked for and the marker is
/// information about what was lost on top of it.
///
/// * `cap == 0` → empty string, no marker, nothing kept.
/// * `0 < cap < 64` → head/tail split 2/3 – 1/3, **no marker** (it does not fit).
/// * `cap >= 64` and `text` longer than `cap` → head/tail split 2/3 – 1/3 with the
///   marker naming how many characters were dropped.
///
/// Splits happen on character boundaries, so multi-byte UTF-8 is never cut in the
/// middle. Used for tool outputs and injections.
pub fn truncate_head_tail(text: &str, cap: usize) -> String {
    if cap == 0 {
        return String::new();
    }
    let total = text.chars().count();
    if total <= cap {
        return text.to_string();
    }
    let head_len = cap * 2 / 3;
    let tail_len = cap - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total - tail_len).collect();
    let dropped = total - head_len - tail_len;
    if cap < TRUNCATION_MARKER_MIN_CAP {
        return format!("{head}{tail}");
    }
    format!("{head}\n\n[... {dropped} characters truncated ...]\n\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_concat_and_tool_uses() {
        let msg = Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Text { text: "a".into() },
                ContentBlock::ToolUse {
                    id: CallId::new("c1"),
                    name: "read".into(),
                    input: serde_json::json!({"path": "x"}),
                },
                ContentBlock::Text { text: "b".into() },
            ],
        );
        assert_eq!(msg.text(), "a\nb");
        assert_eq!(msg.tool_uses().count(), 1);
        assert!(msg.has_tool_uses());
    }

    #[test]
    fn truncation_keeps_head_and_tail() {
        let text: String = (0..1000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let cut = truncate_head_tail(&text, 300);
        assert!(cut.starts_with("abcdefghij"));
        assert!(cut.contains("characters truncated"));
        assert!(cut.chars().count() < 400);
        assert_eq!(truncate_head_tail("short", 300), "short");
    }

    /// A text of `n` distinct chars, so head and tail are recognisable.
    fn ramp(n: usize) -> String {
        (0..n).map(|i| char::from(b'a' + (i % 26) as u8)).collect()
    }

    #[test]
    fn zero_cap_yields_nothing() {
        assert_eq!(truncate_head_tail(&ramp(500), 0), "");
        assert_eq!(truncate_head_tail("", 0), "");
    }

    #[test]
    fn one_cap_keeps_exactly_one_char() {
        // At cap 1 the 2/3 – 1/3 split gives head 0 / tail 1, so what survives
        // is the last character — and a char boundary, not a byte one.
        let text = ramp(500);
        assert_eq!(truncate_head_tail(&text, 1), "f");
        assert_eq!(truncate_head_tail("Привет", 1), "т");
        assert_eq!(truncate_head_tail(&ramp(1), 1), "a");
    }

    #[test]
    fn below_marker_threshold_cuts_without_a_marker() {
        let text = ramp(500);
        for cap in [1usize, 2, 31, 62, 63] {
            let cut = truncate_head_tail(&text, cap);
            assert!(
                !cut.contains("truncated"),
                "cap {cap} must not carry a marker, got {cut:?}"
            );
            assert_eq!(cut.chars().count(), cap, "cap {cap} was not honoured");
        }
    }

    #[test]
    fn at_marker_threshold_carries_the_marker() {
        let text = ramp(500);
        let cut = truncate_head_tail(&text, 64);
        assert!(cut.contains("characters truncated"));
        let head_len = 64 * 2 / 3;
        let tail_len = 64 - head_len;
        assert!(cut.starts_with(&ramp(text.len())[..head_len]));
        assert!(cut.ends_with(&ramp(text.len())[text.len() - tail_len..]));
        // Payload is exactly `cap` chars; the marker is extra, never carved out
        // of the budget.
        let payload: usize = cut.split("\n\n[... ").next().unwrap().chars().count()
            + cut.rsplit_once(" ...]\n\n").unwrap().1.chars().count();
        assert_eq!(payload, 64);
    }

    #[test]
    fn sixty_five_is_still_head_tail_with_marker() {
        let text = ramp(500);
        let cut = truncate_head_tail(&text, 65);
        assert!(cut.contains("characters truncated"));
        assert!(cut.chars().count() > 65);
        assert!(cut.chars().count() < 65 + 64);
    }

    #[test]
    fn text_shorter_than_cap_is_untouched() {
        assert_eq!(truncate_head_tail("short", 300), "short");
        // Exactly `cap` is not truncation either.
        let exact = ramp(64);
        assert_eq!(truncate_head_tail(&exact, 64), exact);
    }

    #[test]
    fn marker_names_the_dropped_character_count() {
        let text = ramp(1000);
        let cap = 300;
        let cut = truncate_head_tail(&text, cap);
        let dropped = 1000 - cap;
        assert!(
            cut.contains(&format!("[... {dropped} characters truncated ...]")),
            "marker must name {dropped}, got {cut:?}"
        );
    }

    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        // Кириллица (2 bytes) and emoji (4 bytes): a byte-wise cut would panic or
        // produce replacement characters here.
        let text: String = (0..300)
            .map(|i| if i % 2 == 0 { 'я' } else { '🎉' })
            .collect();
        for cap in 0..200 {
            let cut = truncate_head_tail(&text, cap);
            // A valid `String` proves no byte was cut mid-char; re-encoding to
            // bytes and back catches a lossy replacement character too.
            assert_eq!(
                String::from_utf8(cut.clone().into_bytes()).expect("valid utf-8"),
                cut
            );
            assert!(!cut.contains('\u{fffd}'), "cap {cap} produced a lossy char");
            if cap > 0 && cap < 64 {
                assert_eq!(cut.chars().count(), cap, "cap {cap} was not honoured");
            }
            assert!(cut.chars().count() <= cap + 64 || cap == 0);
        }
    }

    #[test]
    fn serde_shape() {
        let msg = Message::tool_result(CallId::new("c1"), "ok", false);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"][0]["type"], "tool_result");
        assert_eq!(json["content"][0]["call_id"], "c1");
    }

    #[test]
    fn image_block_serde_roundtrip() {
        let block = ContentBlock::Image {
            source: ImageSource::Path(std::path::PathBuf::from("/tmp/img-1.png")),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["path"], "/tmp/img-1.png");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);

        let block = ContentBlock::Image {
            source: ImageSource::Bytes {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            },
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["source"]["bytes"]["mime"], "image/png");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn image_source_placeholder() {
        assert_eq!(
            ImageSource::Path(std::path::PathBuf::from("shots/a.png")).placeholder(),
            "[image file: shots/a.png]"
        );
        assert_eq!(
            ImageSource::Bytes {
                mime: "image/png".into(),
                data: vec![0; 42],
            }
            .placeholder(),
            "[image: image/png, 42 bytes]"
        );
    }

    #[test]
    fn image_char_len_is_a_fixed_estimate() {
        // A vision model charges ≈1500 tokens per image; the serialized byte
        // length must not count against the window estimate.
        let msg = Message::new(
            Role::User,
            vec![ContentBlock::Image {
                source: ImageSource::Bytes {
                    mime: "image/png".into(),
                    data: vec![0; 1_000_000],
                },
            }],
        );
        assert_eq!(msg.char_len(), IMAGE_ESTIMATE_CHARS);
        assert_eq!(msg.estimate_tokens(), 1500);
    }
}
