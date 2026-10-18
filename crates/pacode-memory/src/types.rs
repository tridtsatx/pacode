//! Public data types and input normalisation.

use crate::error::MemoryError;
use crate::text::{collapse_ws, truncate_chars};

pub const MAX_TITLE_CHARS: usize = 80;
pub const MAX_TEXT_CHARS: usize = 2000;
pub const MAX_TAGS: usize = 8;
pub const MAX_TAG_CHARS: usize = 32;
pub const DEFAULT_RECALL_LIMIT: usize = 5;
pub const MAX_RECALL_LIMIT: usize = 20;
/// Title derived from the text when the caller gives none.
pub const DERIVED_TITLE_CHARS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Shared across all projects (`<config dir>/memory.db`).
    Global,
    /// This project only (`<cwd>/.pacode/memory.db`).
    Project,
}

impl Scope {
    pub const ALL: [Scope; 2] = [Scope::Global, Scope::Project];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
        }
    }

    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "global" => Ok(Scope::Global),
            "project" => Ok(Scope::Project),
            other => Err(MemoryError::invalid(format!(
                "invalid scope '{other}', expected 'global' or 'project'"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Fact,
    Preference,
    Decision,
    Lesson,
    Gotcha,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Fact,
        Kind::Preference,
        Kind::Decision,
        Kind::Lesson,
        Kind::Gotcha,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Preference => "preference",
            Kind::Decision => "decision",
            Kind::Lesson => "lesson",
            Kind::Gotcha => "gotcha",
        }
    }

    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fact" => Ok(Kind::Fact),
            "preference" => Ok(Kind::Preference),
            "decision" => Ok(Kind::Decision),
            "lesson" => Ok(Kind::Lesson),
            "gotcha" => Ok(Kind::Gotcha),
            other => Err(MemoryError::invalid(format!(
                "invalid kind '{other}', expected fact|preference|decision|lesson|gotcha"
            ))),
        }
    }
}

/// A stored memory entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub scope: Scope,
    pub kind: Kind,
    pub title: String,
    pub text: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub last_recalled_ms: i64,
    pub recall_count: i64,
    pub source: Option<String>,
    pub superseded_by: Option<String>,
}

/// Input of a write. Call [`NewEntry::normalize`] (the store does) to validate and cap.
#[derive(Clone, Debug)]
pub struct NewEntry {
    pub kind: Kind,
    pub title: String,
    pub text: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: Option<String>,
    /// Id of an older entry this one replaces (it gets hidden).
    pub supersedes: Option<String>,
}

impl NewEntry {
    pub fn new(kind: Kind, title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            text: text.into(),
            tags: Vec::new(),
            pinned: false,
            source: None,
            supersedes: None,
        }
    }

    /// Validate and cap: title <= 80 chars (derived from the text when empty), text <=
    /// 2000 chars, at most 8 tags of <= 32 chars.
    pub fn normalize(mut self) -> Result<Self, MemoryError> {
        let text = self.text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim();
        if text.is_empty() {
            return Err(MemoryError::invalid("text cannot be empty"));
        }
        self.text = truncate_chars(text, MAX_TEXT_CHARS).trim().to_string();
        let title = collapse_ws(&self.title);
        let title = if title.is_empty() {
            truncate_chars(&collapse_ws(&self.text), DERIVED_TITLE_CHARS)
        } else {
            title
        };
        self.title = truncate_chars(&title, MAX_TITLE_CHARS).trim().to_string();
        let mut tags: Vec<String> = Vec::new();
        for raw in &self.tags {
            let t = normalize_tag(raw);
            if !t.is_empty() && !tags.contains(&t) {
                tags.push(t);
            }
            if tags.len() >= MAX_TAGS {
                break;
            }
        }
        self.tags = tags;
        self.source = self
            .source
            .map(|s| truncate_chars(s.trim(), 80))
            .filter(|s| !s.is_empty());
        self.supersedes = self
            .supersedes
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(self)
    }
}

fn normalize_tag(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        .collect();
    truncate_chars(&cleaned, MAX_TAG_CHARS)
}

/// Result of [`crate::MemoryStore::write`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOutcome {
    pub id: String,
    pub scope: Scope,
    /// `false` when a near-duplicate was found and updated instead of inserting.
    pub created: bool,
    /// `true` when the `supersedes` target existed and was hidden.
    pub superseded: bool,
}

/// Recall parameters. An empty (or token-less) `query` lists pinned + most recent entries.
#[derive(Clone, Debug)]
pub struct RecallRequest {
    pub query: String,
    pub scopes: Vec<Scope>,
    pub kind: Option<Kind>,
    pub limit: usize,
}

impl RecallRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            scopes: Scope::ALL.to_vec(),
            kind: None,
            limit: DEFAULT_RECALL_LIMIT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub entry: Entry,
    /// Final ranking score (0.0 for listing without a query).
    pub score: f64,
}
