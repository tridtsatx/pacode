//! Wire dialects: `ProviderDialect` → adapter factory.
//!
//! A dialect is the *protocol* half of a provider entry (how the request body
//! is shaped, how the stream is decoded); `base_url`, credentials, headers,
//! path and timeouts are the *deployment* half and live in `ProviderConfig`.
//! Keeping the split means a new vendor on an existing protocol is a TOML
//! entry, while a genuinely new protocol is one row in [`DIALECTS`] instead of
//! an edit to every `match` over [`ProviderKind`] across the workspace.
//!
//! Adding a dialect: implement the constructor, add one [`DialectEntry`].
//! Nothing else in the crate needs to know it exists.

use std::collections::BTreeMap;
use std::sync::Arc;

use pacode_types::{Pricing, ProviderConfig, ProviderDefaults, ProviderDialect};

use crate::registry::ResolvedCredential;
use crate::{Provider, ProviderError};

#[cfg(test)]
#[path = "dialect_tests.rs"]
mod dialect_tests;

/// Everything a dialect needs to build one transport, beyond the resolved
/// credential. Split out from [`ResolvedCredential`] so the credential
/// resolution order stays in one place (`resolve_provider_credential`).
#[derive(Clone, Debug, Default)]
pub struct DialectContext {
    pub defaults: ProviderDefaults,
    pub pricing: BTreeMap<String, Pricing>,
}

/// Builds a transport for one dialect. Object-safe so the table can hold
/// `&'static dyn DialectFactory` entries.
pub trait DialectFactory: Send + Sync {
    fn build(
        &self,
        id: &str,
        cfg: &ProviderConfig,
        ctx: &DialectContext,
        credential: &ResolvedCredential,
    ) -> Result<Arc<dyn Provider>, ProviderError>;
}

/// The OpenAI `/chat/completions` dialect — also the universal fallback: any
/// endpoint that speaks it is configurable without touching pacode.
struct OpenAiCompatibleDialect;

impl DialectFactory for OpenAiCompatibleDialect {
    fn build(
        &self,
        id: &str,
        cfg: &ProviderConfig,
        ctx: &DialectContext,
        credential: &ResolvedCredential,
    ) -> Result<Arc<dyn Provider>, ProviderError> {
        let provider = crate::openai_compat::OpenAiCompat::new(
            id.to_string(),
            cfg.clone(),
            ctx.defaults.clone(),
            credential.api_key().map(String::from),
            ctx.pricing.clone(),
        )?;
        Ok(Arc::new(provider))
    }
}

struct AnthropicDialect;

impl DialectFactory for AnthropicDialect {
    fn build(
        &self,
        id: &str,
        cfg: &ProviderConfig,
        ctx: &DialectContext,
        credential: &ResolvedCredential,
    ) -> Result<Arc<dyn Provider>, ProviderError> {
        let provider = match credential {
            ResolvedCredential::StoreOAuth(acc) => crate::anthropic::Anthropic::from_oauth(
                id.to_string(),
                cfg.clone(),
                ctx.defaults.clone(),
                acc.access.clone(),
                ctx.pricing.clone(),
            )?,
            _ => crate::anthropic::Anthropic::from_api_key(
                id.to_string(),
                cfg.clone(),
                ctx.defaults.clone(),
                credential.api_key().map(String::from),
                ctx.pricing.clone(),
            )?,
        };
        Ok(Arc::new(provider))
    }
}

struct CodexDialect;

impl DialectFactory for CodexDialect {
    fn build(
        &self,
        id: &str,
        cfg: &ProviderConfig,
        ctx: &DialectContext,
        credential: &ResolvedCredential,
    ) -> Result<Arc<dyn Provider>, ProviderError> {
        let provider = match credential {
            ResolvedCredential::StoreOAuth(acc) => {
                let tokens = crate::codex::CodexOAuthTokens {
                    access_token: acc.access.clone(),
                    refresh_token: acc.refresh.clone(),
                    id_token: acc
                        .extra
                        .get("id_token")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    account_id: acc
                        .extra
                        .get("account_id")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                };
                crate::codex::Codex::new(
                    id.to_string(),
                    cfg.clone(),
                    ctx.defaults.clone(),
                    Some(crate::codex::CodexAuth::oauth_tokens(tokens)),
                    ctx.pricing.clone(),
                )?
            }
            _ => crate::codex::Codex::from_api_key(
                id.to_string(),
                cfg.clone(),
                ctx.defaults.clone(),
                credential.api_key().map(String::from),
                ctx.pricing.clone(),
            )?,
        };
        Ok(Arc::new(provider))
    }
}

struct DevinDialect;

impl DialectFactory for DevinDialect {
    fn build(
        &self,
        id: &str,
        cfg: &ProviderConfig,
        ctx: &DialectContext,
        credential: &ResolvedCredential,
    ) -> Result<Arc<dyn Provider>, ProviderError> {
        // The Devin credential is a pair: the stored key plus the session token
        // the login handed out. `api_server_url` is per account, so it comes
        // from the credential rather than from the static config.
        let (api_key, session_token, api_server_url) = match credential {
            ResolvedCredential::StoreOAuth(acc) => {
                let extra = |k: &str| acc.extra.get(k).and_then(|v| v.as_str()).map(String::from);
                (
                    Some(acc.access.clone()),
                    extra("session_token").or_else(|| Some(acc.access.clone())),
                    extra("api_server_url"),
                )
            }
            _ => (credential.api_key().map(String::from), None, None),
        };
        let provider = crate::devin::Devin::new(
            id.to_string(),
            cfg.clone(),
            ctx.defaults.clone(),
            api_key,
            session_token,
            api_server_url,
            ctx.pricing.clone(),
        )?;
        Ok(Arc::new(provider))
    }
}

struct OpenCodeZenDialect;

impl DialectFactory for OpenCodeZenDialect {
    fn build(
        &self,
        id: &str,
        cfg: &ProviderConfig,
        ctx: &DialectContext,
        credential: &ResolvedCredential,
    ) -> Result<Arc<dyn Provider>, ProviderError> {
        // OpenCode Zen: OpenAI-compatible with `api_key = "public"` and
        // opencode-specific headers injected via ProviderConfig.headers.
        // Catalog comes from api.json, not from `/models`.
        let api_key = credential
            .api_key()
            .map(String::from)
            .or_else(|| Some("public".to_string()));
        let provider = crate::opencode::OpenCodeProvider::new(
            id.to_string(),
            cfg.clone(),
            ctx.defaults.clone(),
            api_key,
            ctx.pricing.clone(),
        )?;
        Ok(Arc::new(provider))
    }
}

/// One row of the dialect table.
struct DialectEntry {
    dialect: ProviderDialect,
    /// Every factory is a stateless unit struct, so a `'static` borrow costs
    /// nothing to obtain and needs no lazy init or lock to reach.
    factory: &'static dyn DialectFactory,
    /// Whether an empty `base_url` is acceptable. Codex derives its host from
    /// the credential, so a synthesized `openai` entry may leave it blank.
    base_url_optional: bool,
}

/// The dialect table. This is the *only* place a dialect name maps to code;
/// everything else in the workspace asks [`build_transport`] for a transport and
/// never matches on the dialect itself.
static DIALECTS: &[DialectEntry] = &[
    DialectEntry {
        dialect: ProviderDialect::OpenAiCompatible,
        factory: &OpenAiCompatibleDialect,
        base_url_optional: false,
    },
    DialectEntry {
        dialect: ProviderDialect::Anthropic,
        factory: &AnthropicDialect,
        base_url_optional: false,
    },
    DialectEntry {
        dialect: ProviderDialect::Codex,
        factory: &CodexDialect,
        base_url_optional: true,
    },
    DialectEntry {
        dialect: ProviderDialect::Devin,
        factory: &DevinDialect,
        base_url_optional: false,
    },
    DialectEntry {
        dialect: ProviderDialect::OpenCodeZen,
        factory: &OpenCodeZenDialect,
        base_url_optional: false,
    },
];

/// Every dialect pacode can serve. A closed set by construction: the enum
/// cannot name a dialect that has no adapter, so an unimplemented protocol is
/// a compile error rather than a runtime 404.
pub fn known_dialects() -> impl Iterator<Item = ProviderDialect> {
    DIALECTS.iter().map(|entry| entry.dialect)
}

fn entry(dialect: ProviderDialect) -> &'static DialectEntry {
    DIALECTS
        .iter()
        .find(|entry| entry.dialect == dialect)
        // Unreachable while `known_dialects` covers the enum, and covered by
        // `every_dialect_has_a_factory`; a silent default would be worse.
        .unwrap_or_else(|| {
            unreachable!("dialect {} has no factory", dialect.as_str());
        })
}

/// Build the transport for `cfg` under its dialect.
pub fn build_transport(
    id: &str,
    cfg: &ProviderConfig,
    ctx: &DialectContext,
    credential: &ResolvedCredential,
) -> Result<Arc<dyn Provider>, ProviderError> {
    entry(cfg.effective_dialect())
        .factory
        .build(id, cfg, ctx, credential)
}

/// Whether this entry may leave `base_url` empty (Codex fills it in from the
/// stored credential; every other dialect needs a host).
pub fn base_url_optional(dialect: ProviderDialect) -> bool {
    entry(dialect).base_url_optional
}
