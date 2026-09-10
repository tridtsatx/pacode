//! Inline tool call parser for Devin API responses.
//!
//! Devin models stream tool calls as `functions.<name>:<index>{"arg": ...}` within ordinary text.
//! Text before such a prefix is emitted as assistant text. If a potential tool call prefix is
//! detected, the parser buffers until the complete JSON arguments object is received and parses
//! successfully before emitting [`StreamEvent::ToolCallStart`] and [`StreamEvent::ToolCallArgsDelta`].

use pacode_types::{CallId, StreamEvent};
use serde_json::Value;

const FUNCTION_PREFIXES: &[&str] = &[
    "functions",
    "function",
    "functio",
    "funct",
    "func",
    "fun",
    "fu",
    "f",
];

/// Find the index right after the balanced closing `}` of a JSON object.
pub fn find_json_object_end(input: &str) -> Option<usize> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first != '{' {
        return None;
    }
    let mut depth = 1;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in chars {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(idx + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Accumulator and detector for inline tool calls in `delta_text`.
#[derive(Default, Debug)]
pub struct InlineToolCallParser {
    buffer: String,
    any_tool_call: bool,
}

impl InlineToolCallParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            any_tool_call: false,
        }
    }

    pub fn has_tool_calls(&self) -> bool {
        self.any_tool_call
    }

    pub fn feed(&mut self, delta: &str) -> Vec<StreamEvent> {
        self.buffer.push_str(delta);
        self.drain(false)
    }

    pub fn flush(&mut self) -> Vec<StreamEvent> {
        self.drain(true)
    }

    fn drain(&mut self, is_eof: bool) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        loop {
            if self.buffer.is_empty() {
                break;
            }

            if let Some(pos) = self.buffer.find("functions.") {
                // Emit ordinary text preceding "functions."
                if pos > 0 {
                    events.push(StreamEvent::TextDelta {
                        text: self.buffer[..pos].to_string(),
                    });
                    self.buffer.drain(..pos);
                }

                let after_tag = &self.buffer["functions.".len()..];

                // Check tool name before ':'
                if let Some(colon_pos) = after_tag.find(':') {
                    let tool_name = &after_tag[..colon_pos];
                    if tool_name.is_empty()
                        || !tool_name
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        // Invalid tool name — ordinary text
                        events.push(StreamEvent::TextDelta {
                            text: "functions.".to_string(),
                        });
                        self.buffer.drain(.."functions.".len());
                        continue;
                    }

                    // Check index before '{'
                    let after_colon = &after_tag[colon_pos + 1..];
                    if let Some(brace_pos) = after_colon.find('{') {
                        let idx_str = &after_colon[..brace_pos];
                        if idx_str.is_empty() || !idx_str.chars().all(|c| c.is_ascii_digit()) {
                            // Invalid index — ordinary text
                            events.push(StreamEvent::TextDelta {
                                text: "functions.".to_string(),
                            });
                            self.buffer.drain(.."functions.".len());
                            continue;
                        }

                        let index: u32 = idx_str.parse().unwrap_or(0);
                        let json_start = "functions.".len() + colon_pos + 1 + brace_pos;
                        let json_slice = &self.buffer[json_start..];

                        if let Some(json_len) = find_json_object_end(json_slice) {
                            let json_str = &json_slice[..json_len];
                            if serde_json::from_str::<Value>(json_str).is_ok() {
                                self.any_tool_call = true;
                                events.push(StreamEvent::ToolCallStart {
                                    index,
                                    id: CallId::generate(),
                                    name: tool_name.to_string(),
                                });
                                events.push(StreamEvent::ToolCallArgsDelta {
                                    index,
                                    delta: json_str.to_string(),
                                });

                                let total_len = json_start + json_len;
                                self.buffer.drain(..total_len);
                                continue;
                            } else {
                                events.push(StreamEvent::TextDelta {
                                    text: "functions.".to_string(),
                                });
                                self.buffer.drain(.."functions.".len());
                                continue;
                            }
                        } else if is_eof {
                            // Incomplete JSON at EOF: flush as ordinary text
                            events.push(StreamEvent::TextDelta {
                                text: std::mem::take(&mut self.buffer),
                            });
                            break;
                        } else {
                            break;
                        }
                    } else if after_colon.chars().all(|c| c.is_ascii_digit()) {
                        if is_eof {
                            events.push(StreamEvent::TextDelta {
                                text: std::mem::take(&mut self.buffer),
                            });
                            break;
                        } else {
                            break;
                        }
                    } else {
                        events.push(StreamEvent::TextDelta {
                            text: "functions.".to_string(),
                        });
                        self.buffer.drain(.."functions.".len());
                        continue;
                    }
                } else if after_tag
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    if is_eof {
                        events.push(StreamEvent::TextDelta {
                            text: std::mem::take(&mut self.buffer),
                        });
                        break;
                    } else {
                        break;
                    }
                } else {
                    events.push(StreamEvent::TextDelta {
                        text: "functions.".to_string(),
                    });
                    self.buffer.drain(.."functions.".len());
                    continue;
                }
            } else if !is_eof {
                let mut matched_prefix = false;
                for &prefix in FUNCTION_PREFIXES {
                    if self.buffer.ends_with(prefix) {
                        let emit_len = self.buffer.len() - prefix.len();
                        if emit_len > 0 {
                            events.push(StreamEvent::TextDelta {
                                text: self.buffer[..emit_len].to_string(),
                            });
                            self.buffer.drain(..emit_len);
                        }
                        matched_prefix = true;
                        break;
                    }
                }
                if !matched_prefix {
                    events.push(StreamEvent::TextDelta {
                        text: std::mem::take(&mut self.buffer),
                    });
                }
                break;
            } else {
                events.push(StreamEvent::TextDelta {
                    text: std::mem::take(&mut self.buffer),
                });
                break;
            }
        }

        events
    }
}
