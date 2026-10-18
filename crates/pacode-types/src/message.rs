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
    /// An image. Only metadata lives here: the bytes are stored once, content-addressed
    /// by `hash`, in the image store (`pacode-image`) and loaded lazily by whoever needs
    /// pixels. In a `Role::Tool` message an `Image` block belongs to the tool result
    /// that precedes it.
    Image {
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

impl ContentBlock {
    /// Block for a stored image.
    pub fn image(image: &crate::ImageRef) -> Self {
        ContentBlock::Image {
            mime: image.mime.clone(),
            hash: image.hash.clone(),
            source_path: image.name.clone(),
            width: image.width,
            height: image.height,
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
                ContentBlock::Image {
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
                // Fixed estimate (4 chars per token), never the payload size.
                ContentBlock::Image { width, height, .. } => {
                    crate::estimate_image_tokens(*width, *height) as usize * 4
                }
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

/// Cut `text` to at most `cap` characters keeping head and tail, with a marker
/// describing how much was dropped. Used for tool outputs and injections.
pub fn truncate_head_tail(text: &str, cap: usize) -> String {
    let total = text.chars().count();
    if total <= cap || cap < 64 {
        return text.to_string();
    }
    let head_len = cap * 2 / 3;
    let tail_len = cap - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total.saturating_sub(tail_len)).collect();
    let dropped = total - head_len - tail_len;
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

    #[test]
    fn serde_shape() {
        let msg = Message::tool_result(CallId::new("c1"), "ok", false);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"][0]["type"], "tool_result");
        assert_eq!(json["content"][0]["call_id"], "c1");
    }
}
