//! Where a marketplace comes from, and how its files are fetched.

use std::time::Duration;

use async_trait::async_trait;

/// Longest response accepted for an index or an archive. Everything from the
/// network is bounded before it reaches memory.
pub const INDEX_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const ARCHIVE_MAX_BYTES: usize = 64 * 1024 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A marketplace's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarketplaceSource {
    /// A GitHub repository, optionally pinned to a ref.
    GitHub {
        owner: String,
        repo: String,
        /// Branch, tag or commit. `None` means the repository's default branch.
        reference: Option<String>,
    },
    /// A direct https URL to a marketplace.json.
    Url(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("cannot read the source {0:?}: expected owner/repo, owner/repo@ref or an https URL")]
    Unrecognised(String),
    #[error("the source contains a character that is not allowed")]
    BadCharacter,
}

impl MarketplaceSource {
    /// Parse `owner/repo`, `owner/repo@ref`, or an https URL.
    pub fn parse(spec: &str) -> Result<Self, SourceError> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err(SourceError::Unrecognised(spec.to_string()));
        }
        if spec.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(SourceError::BadCharacter);
        }

        if spec.starts_with("https://") {
            // A URL is taken as written; the fetcher bounds what it returns.
            return Ok(Self::Url(spec.to_string()));
        }
        if spec.starts_with("http://") {
            // Plain http would let anyone on the path choose what gets installed.
            return Err(SourceError::Unrecognised(spec.to_string()));
        }

        let (path, reference) = match spec.split_once('@') {
            Some((path, r)) if !r.is_empty() => (path, Some(r.to_string())),
            Some(_) => return Err(SourceError::Unrecognised(spec.to_string())),
            None => (spec, None),
        };
        let Some((owner, repo)) = path.split_once('/') else {
            return Err(SourceError::Unrecognised(spec.to_string()));
        };
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return Err(SourceError::Unrecognised(spec.to_string()));
        }
        if !is_safe_segment(owner) || !is_safe_segment(repo) {
            return Err(SourceError::BadCharacter);
        }
        if let Some(r) = &reference
            && !is_safe_ref(r)
        {
            return Err(SourceError::BadCharacter);
        }

        Ok(Self::GitHub {
            owner: owner.to_string(),
            repo: repo.to_string(),
            reference,
        })
    }

    /// Stable key for the on-disk cache: safe as a file name by construction.
    pub fn cache_key(&self) -> String {
        match self {
            Self::GitHub {
                owner,
                repo,
                reference,
            } => match reference {
                Some(r) => format!("gh_{owner}_{repo}_{}", sanitise(r)),
                None => format!("gh_{owner}_{repo}"),
            },
            Self::Url(url) => format!("url_{}", sanitise(url)),
        }
    }

    /// How the source reads back to a person.
    pub fn display(&self) -> String {
        match self {
            Self::GitHub {
                owner,
                repo,
                reference,
            } => match reference {
                Some(r) => format!("{owner}/{repo}@{r}"),
                None => format!("{owner}/{repo}"),
            },
            Self::Url(url) => url.clone(),
        }
    }

    /// Where the index lives.
    pub fn index_url(&self) -> String {
        match self {
            Self::GitHub {
                owner,
                repo,
                reference,
            } => {
                let r = reference.as_deref().unwrap_or("HEAD");
                format!(
                    "https://raw.githubusercontent.com/{owner}/{repo}/{r}/.claude-plugin/marketplace.json"
                )
            }
            Self::Url(url) => url.clone(),
        }
    }

    /// Where the repository archive lives, when the source is a repository.
    pub fn archive_url(&self) -> Option<String> {
        match self {
            Self::GitHub {
                owner,
                repo,
                reference,
            } => {
                let r = reference.as_deref().unwrap_or("HEAD");
                Some(format!(
                    "https://codeload.github.com/{owner}/{repo}/tar.gz/{r}"
                ))
            }
            Self::Url(_) => None,
        }
    }
}

fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_safe_ref(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        && !s.starts_with('/')
}

fn sanitise(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("{url} answered {status}")]
    Status { url: String, status: u16 },
    #[error("{url} returned more than {limit} bytes")]
    TooLarge { url: String, limit: usize },
}

/// Fetches bytes for the marketplace. Injectable so tests never touch the network.
#[async_trait]
pub trait MarketplaceFetcher: Send + Sync {
    async fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, FetchError>;
}

/// The real fetcher, over the workspace's HTTP client.
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .user_agent(concat!("pacode/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MarketplaceFetcher for HttpFetcher {
    async fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, FetchError> {
        let mut request = self.client.get(url);
        // Public repositories need no token; one is used when present purely to
        // lift the anonymous rate limit.
        if let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
            && url.contains("github")
        {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }

        // A declared length past the cap is refused before a byte is read.
        if let Some(len) = response.content_length()
            && len as usize > limit
        {
            return Err(FetchError::TooLarge {
                url: url.to_string(),
                limit,
            });
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if bytes.len() > limit {
            return Err(FetchError::TooLarge {
                url: url.to_string(),
                limit,
            });
        }
        Ok(bytes.to_vec())
    }
}
