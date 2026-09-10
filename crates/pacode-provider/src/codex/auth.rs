//! Authentication types and JWT claims extraction for Codex / OpenAI Responses API.

use serde_json::Value;

/// Stored or active OAuth tokens for ChatGPT Codex authentication.
#[derive(Clone, Debug, PartialEq)]
pub struct CodexOAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
}

/// Authentication credentials for the Codex provider: either a plain OpenAI API key
/// or ChatGPT OAuth credentials.
#[derive(Clone, Debug, PartialEq)]
pub enum CodexAuth {
    ApiKey(String),
    OAuth(CodexOAuthTokens),
}

impl CodexAuth {
    /// Construct API-key authentication.
    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey(key.into())
    }

    /// Construct OAuth authentication from an access token.
    pub fn oauth(access_token: impl Into<String>) -> Self {
        Self::OAuth(CodexOAuthTokens {
            access_token: access_token.into(),
            refresh_token: None,
            id_token: None,
            account_id: None,
        })
    }

    /// Construct OAuth authentication with full tokens.
    pub fn oauth_tokens(tokens: CodexOAuthTokens) -> Self {
        Self::OAuth(tokens)
    }

    /// `true` if this instance represents ChatGPT OAuth authentication.
    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::OAuth(_))
    }

    /// The token string to send in the `Authorization: Bearer <token>` header.
    pub fn bearer_token(&self) -> &str {
        match self {
            Self::ApiKey(key) => key.as_str(),
            Self::OAuth(tokens) => tokens.access_token.as_str(),
        }
    }

    /// The ChatGPT account ID, if known or extractable from JWT tokens.
    pub fn account_id(&self) -> Option<String> {
        match self {
            Self::ApiKey(_) => None,
            Self::OAuth(tokens) => tokens
                .account_id
                .clone()
                .or_else(|| tokens.id_token.as_deref().and_then(extract_account_id))
                .or_else(|| extract_account_id(&tokens.access_token)),
        }
    }
}

/// Extract the `chatgpt_account_id` claim from a JWT token (typically an `id_token`).
///
/// Ported from jcode: reads `https://api.openai.com/auth` -> `chatgpt_account_id`.
pub fn extract_account_id(token: &str) -> Option<String> {
    let payload = decode_jwt_payload(token)?;
    let auth = payload.get("https://api.openai.com/auth")?;
    auth.get("chatgpt_account_id")?
        .as_str()
        .map(|value| value.to_string())
}

/// Decode the JSON payload section of a JWT (the second dot-delimited segment).
fn decode_jwt_payload(token: &str) -> Option<Value> {
    let payload_b64 = token.split('.').nth(1)?;
    let decoded = decode_base64_url(payload_b64)?;
    serde_json::from_slice::<Value>(&decoded).ok()
}

/// Decode a URL-safe Base64 string (with or without padding).
pub fn decode_base64_url(input: &str) -> Option<Vec<u8>> {
    let mut s = input.replace('-', "+").replace('_', "/");
    match s.len() % 4 {
        2 => s.push_str("=="),
        3 => s.push('='),
        0 => {}
        _ => return None,
    }

    let chars = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);

    let decode_char = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    };

    for chunk in chars.chunks(4) {
        if chunk.len() != 4 {
            return None;
        }
        let b0 = decode_char(chunk[0])?;
        let b1 = decode_char(chunk[1])?;
        let b2 = decode_char(chunk[2])?;
        let b3 = decode_char(chunk[3])?;

        out.push((b0 << 2) | (b1 >> 4));
        if chunk[2] != b'=' {
            out.push((b1 << 4) | (b2 >> 2));
        }
        if chunk[3] != b'=' {
            out.push((b2 << 6) | b3);
        }
    }

    Some(out)
}
