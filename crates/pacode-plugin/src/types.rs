//! Common types for pacode plugins.

use std::path::PathBuf;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Lua,
    Wasm,
}

impl std::fmt::Display for PluginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lua => write!(f, "lua"),
            Self::Wasm => write!(f, "wasm"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default)]
    pub description: Option<String>,
}

impl PluginManifest {
    pub fn entry_path(&self) -> PathBuf {
        self.entry.clone().unwrap_or_else(|| match self.kind {
            PluginKind::Lua => PathBuf::from("main.lua"),
            PluginKind::Wasm => PathBuf::from("main.wasm"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub schema: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCommandDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "text", rename_all = "snake_case")]
pub enum CommandOutcome {
    InsertText(String),
    SendPrompt(String),
    Nothing,
}

impl<'de> Deserialize<'de> for CommandOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let val = Value::deserialize(deserializer)?;
        match val {
            Value::Null => Ok(Self::Nothing),
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "nothing" => Ok(Self::Nothing),
                _ => Ok(Self::InsertText(s)),
            },
            Value::Object(map) => {
                if let Some(Value::String(s)) =
                    map.get("insert_text").or_else(|| map.get("InsertText"))
                {
                    return Ok(Self::InsertText(s.clone()));
                }
                if let Some(Value::String(s)) =
                    map.get("send_prompt").or_else(|| map.get("SendPrompt"))
                {
                    return Ok(Self::SendPrompt(s.clone()));
                }
                if let Some(Value::String(typ)) = map.get("type") {
                    let text = map
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    match typ.to_ascii_lowercase().as_str() {
                        "insert_text" => return Ok(Self::InsertText(text)),
                        "send_prompt" => return Ok(Self::SendPrompt(text)),
                        "nothing" => return Ok(Self::Nothing),
                        _ => {}
                    }
                }
                if map.contains_key("nothing") || map.contains_key("Nothing") {
                    return Ok(Self::Nothing);
                }
                Ok(Self::Nothing)
            }
            _ => Ok(Self::Nothing),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookEvent {
    PreToolCall {
        name: String,
        input: Value,
    },
    PostToolCall {
        name: String,
        input: Value,
        output: Value,
    },
    TurnStart,
    TurnEnd {
        duration_ms: u64,
        output_tokens: u64,
    },
    OnMessage {
        role: String,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookResult {
    Continue,
    Deny { reason: String },
    ModifyInput(Value),
}

impl<'de> Deserialize<'de> for HookResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let val = Value::deserialize(deserializer)?;
        match val {
            Value::Null => Ok(Self::Continue),
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "continue" => Ok(Self::Continue),
                _ => Ok(Self::Deny { reason: s }),
            },
            Value::Object(map) => {
                if let Some(deny_val) = map.get("deny").or_else(|| map.get("Deny")) {
                    let reason = match deny_val {
                        Value::String(r) => r.clone(),
                        Value::Object(sub) => sub
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("denied")
                            .to_string(),
                        _ => deny_val.to_string(),
                    };
                    return Ok(Self::Deny { reason });
                }
                if let Some(input_val) = map.get("modify_input").or_else(|| map.get("ModifyInput"))
                {
                    return Ok(Self::ModifyInput(input_val.clone()));
                }
                if let Some(Value::String(typ)) = map.get("type") {
                    match typ.to_ascii_lowercase().as_str() {
                        "continue" => return Ok(Self::Continue),
                        "deny" => {
                            let reason = map
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("denied")
                                .to_string();
                            return Ok(Self::Deny { reason });
                        }
                        "modify_input" | "modify" => {
                            let input = map.get("input").cloned().unwrap_or(Value::Null);
                            return Ok(Self::ModifyInput(input));
                        }
                        _ => {}
                    }
                }
                if map.contains_key("continue") {
                    return Ok(Self::Continue);
                }
                Ok(Self::Continue)
            }
            _ => Ok(Self::Continue),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub kind: Option<PluginKind>,
    pub description: Option<String>,
    pub tools: Vec<PluginToolDef>,
    pub commands: Vec<PluginCommandDef>,
    pub error: Option<String>,
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;
