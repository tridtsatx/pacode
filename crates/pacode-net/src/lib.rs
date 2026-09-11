//! Networking and proxy configuration for pacode.
//!
//! Provides proxy settings resolution and reqwest client builder construction
//! with support for HTTP, HTTPS, SOCKS5, and SOCKS5H proxies.

use std::fmt;
use std::str::FromStr;

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod proxy_tests;

/// Accepted proxy URL schemes.
const ACCEPTED_SCHEMES: &[&str] = &["http", "https", "socks5", "socks5h"];

/// Errors produced when parsing proxy settings or configuring HTTP clients.
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("invalid proxy URL '{value}': {reason}")]
    InvalidUrl { value: String, reason: String },
    #[error(
        "unsupported proxy scheme '{scheme}' in '{value}' (expected http, https, socks5, or socks5h)"
    )]
    UnsupportedScheme { scheme: String, value: String },
    #[error("failed to configure proxy for '{proxy}': {source}")]
    ClientBuilder {
        proxy: String,
        #[source]
        source: reqwest::Error,
    },
}

/// Parsed proxy URL with scheme, host, port, and optional credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyUrl {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    url: url::Url,
}

impl ProxyUrl {
    /// Parse a proxy URL string.
    pub fn parse(raw: &str) -> Result<Self, ProxyError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ProxyError::InvalidUrl {
                value: raw.to_string(),
                reason: "empty proxy URL".to_string(),
            });
        }

        let parsed = url::Url::parse(trimmed).map_err(|e| ProxyError::InvalidUrl {
            value: raw.to_string(),
            reason: e.to_string(),
        })?;

        let scheme = parsed.scheme().to_ascii_lowercase();
        if !ACCEPTED_SCHEMES.contains(&scheme.as_str()) {
            return Err(ProxyError::UnsupportedScheme {
                scheme,
                value: raw.to_string(),
            });
        }

        let host = parsed.host_str().ok_or_else(|| ProxyError::InvalidUrl {
            value: raw.to_string(),
            reason: "missing host in proxy URL".to_string(),
        })?;
        if host.is_empty() {
            return Err(ProxyError::InvalidUrl {
                value: raw.to_string(),
                reason: "empty host in proxy URL".to_string(),
            });
        }
        let host = host.to_string();

        let port = parsed.port();
        let username = if parsed.username().is_empty() {
            None
        } else {
            Some(parsed.username().to_string())
        };
        let password = parsed.password().map(ToString::to_string);

        Ok(Self {
            scheme,
            host,
            port,
            username,
            password,
            url: parsed,
        })
    }

    /// Return the proxy URL as a string slice.
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Return a reference to the parsed [`url::Url`].
    pub fn url(&self) -> &url::Url {
        &self.url
    }
}

impl fmt::Display for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProxyUrl {
    type Err = ProxyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Resolved proxy configuration for a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxySetting {
    /// Inherit proxy settings from the environment (HTTP_PROXY / HTTPS_PROXY / ALL_PROXY).
    Inherit,
    /// Suppress all proxies, including environment variables (`.no_proxy()`).
    Disabled,
    /// Explicitly configured proxy URL (ignores `NO_PROXY`).
    Explicit(ProxyUrl),
}

impl ProxySetting {
    /// Parse raw configuration value:
    /// - `None` (key absent) -> `Inherit`
    /// - `"none"` (case-insensitive) -> `Disabled`
    /// - anything else parsed as a URL -> `Explicit`
    pub fn parse(raw: Option<&str>) -> Result<Self, ProxyError> {
        match raw {
            None => Ok(Self::Inherit),
            Some(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(ProxyError::InvalidUrl {
                        value: s.to_string(),
                        reason: "empty proxy configuration".to_string(),
                    });
                }
                if trimmed.eq_ignore_ascii_case("none") {
                    Ok(Self::Disabled)
                } else {
                    let url = ProxyUrl::parse(trimmed)?;
                    Ok(Self::Explicit(url))
                }
            }
        }
    }

    /// Resolve per-provider value against the global default.
    ///
    /// Precedence:
    /// - `provider` if specified (Some)
    /// - else `global` if specified (Some)
    /// - else `Inherit`
    pub fn resolve(provider: Option<&str>, global: Option<&str>) -> Result<Self, ProxyError> {
        if let Some(p) = provider {
            Self::parse(Some(p))
        } else {
            Self::parse(global)
        }
    }
}

/// Build a `reqwest::ClientBuilder` with the proxy setting applied.
/// The caller adds its own timeouts and custom settings.
pub fn client_builder(setting: &ProxySetting) -> Result<reqwest::ClientBuilder, ProxyError> {
    match setting {
        ProxySetting::Inherit => Ok(reqwest::Client::builder()),
        ProxySetting::Disabled => Ok(reqwest::Client::builder().no_proxy()),
        ProxySetting::Explicit(proxy_url) => {
            let proxy = reqwest::Proxy::all(proxy_url.as_str()).map_err(|source| {
                ProxyError::ClientBuilder {
                    proxy: proxy_url.to_string(),
                    source,
                }
            })?;
            Ok(reqwest::Client::builder().proxy(proxy))
        }
    }
}
