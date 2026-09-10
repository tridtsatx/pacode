//! Types and constants for the Anthropic Messages API transport.

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const CLAUDE_CLI_USER_AGENT: &str = "claude-cli/1.0.0";
pub const OAUTH_BETA: &str = "oauth-2025-04-20,claude-code-20250219";
pub const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8192;

const REASONING_KEYWORDS: &[&str] = &[
    "3-7", "3.7", "opus-4", "sonnet-4", "thinking", "reason", "fable", "mythos",
];

/// Authentication mode for Anthropic requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnthropicAuth {
    ApiKey(String),
    OAuth(String),
}

impl AnthropicAuth {
    pub fn is_oauth(&self) -> bool {
        match self {
            Self::OAuth(_) => true,
            Self::ApiKey(_) => false,
        }
    }

    pub fn token(&self) -> &str {
        match self {
            Self::ApiKey(s) | Self::OAuth(s) => s,
        }
    }

    /// Auto-detect auth mode based on token prefix:
    /// Claude Code OAuth access tokens typically start with `sk-ant-acc` or `session_`.
    /// Standard Anthropic API keys start with `sk-ant-api` (or other key prefixes).
    pub fn detect(token: impl Into<String>) -> Self {
        let t = token.into();
        if t.starts_with("sk-ant-acc") || t.starts_with("session_") {
            Self::OAuth(t)
        } else {
            Self::ApiKey(t)
        }
    }
}

impl From<String> for AnthropicAuth {
    fn from(s: String) -> Self {
        Self::detect(s)
    }
}

impl From<&str> for AnthropicAuth {
    fn from(s: &str) -> Self {
        Self::detect(s)
    }
}

/// Remap tool names on the wire for Claude Code OAuth direct API compatibility.
pub fn map_tool_name_for_oauth(name: &str) -> String {
    match name {
        "bash" => "Bash",
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "glob" => "Glob",
        "grep" => "Grep",
        "subagent" => "Agent",
        "schedule" => "ScheduleWakeup",
        "skill_manage" => "Skill",
        other => other,
    }
    .to_string()
}

/// Reverse map tool names from Claude Code OAuth responses to native names.
pub fn map_tool_name_from_oauth(name: &str) -> String {
    match name {
        "Bash" => "bash",
        "Read" => "read",
        "Write" => "write",
        "Edit" => "edit",
        "Glob" => "glob",
        "Grep" => "grep",
        "Agent" => "subagent",
        "ScheduleWakeup" => "schedule",
        "Skill" => "skill_manage",
        other => other,
    }
    .to_string()
}

pub fn model_heuristic_reasoning(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    REASONING_KEYWORDS.iter().any(|&k| lower.contains(k))
}
