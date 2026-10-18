//! Anchors, operations and errors of the hashline edit model.

use std::fmt;

use serde::Deserialize;
use thiserror::Error;

use super::hash::{decode, encode, hash_code};

/// A position in the file as the model saw it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Anchor {
    /// Before the first line (valid for inserts only; works on an empty file).
    Bof,
    /// After the last line (valid for inserts only; works on an empty file).
    Eof,
    /// Line `line` (1-based) whose content hashed to `hash` when read.
    Line { line: usize, hash: String },
}

impl Anchor {
    /// Parse `12#ab`, `12#ab| text` (a pasted display row), `BOF` or `EOF`.
    pub fn parse(s: &str) -> Result<Self, EditError> {
        let bad = || EditError::InvalidAnchor(s.to_string());
        let head = s.split('|').next().unwrap_or(s).trim();
        if head.eq_ignore_ascii_case("bof") {
            return Ok(Self::Bof);
        }
        if head.eq_ignore_ascii_case("eof") {
            return Ok(Self::Eof);
        }
        let (num, hash) = head.split_once('#').ok_or_else(bad)?;
        let line: usize = num.trim().parse().map_err(|_| bad())?;
        let code = decode(hash.trim()).ok_or_else(bad)?;
        if line == 0 {
            return Err(bad());
        }
        Ok(Self::Line {
            line,
            hash: encode(code),
        })
    }

    /// Anchor for line `line` (1-based) holding `text`.
    pub fn for_line(line: usize, text: &str) -> Self {
        Self::Line {
            line,
            hash: encode(hash_code(text)),
        }
    }
}

impl TryFrom<String> for Anchor {
    type Error = EditError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bof => f.write_str("BOF"),
            Self::Eof => f.write_str("EOF"),
            Self::Line { line, hash } => write!(f, "{line}#{hash}"),
        }
    }
}

/// One edit operation. Every anchor refers to the file as last read; ops never see each
/// other's effects. `lines` hold the new text without terminators.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Replace lines `start..=end` (`end` defaults to `start`) with `lines`.
    Replace {
        start: Anchor,
        #[serde(default)]
        end: Option<Anchor>,
        lines: Vec<String>,
    },
    /// Insert `lines` after the anchored line (`EOF` appends, `BOF` prepends).
    InsertAfter { at: Anchor, lines: Vec<String> },
    /// Insert `lines` before the anchored line (`BOF` prepends, `EOF` appends).
    InsertBefore { at: Anchor, lines: Vec<String> },
    /// Delete lines `start..=end` (`end` defaults to `start`).
    Delete {
        start: Anchor,
        #[serde(default)]
        end: Option<Anchor>,
    },
}

/// An anchor that did not match the current content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleAnchor {
    /// 1-based op number.
    pub op: usize,
    /// `"start"`, `"end"` or `"at"`.
    pub role: &'static str,
    /// The anchor as given.
    pub anchor: String,
    /// Why it was rejected.
    pub reason: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EditError {
    #[error("no operations given")]
    NoOps,
    #[error("invalid anchor {0:?}: expected `LINE#HASH` (e.g. `12#ab`), `BOF` or `EOF`")]
    InvalidAnchor(String),
    #[error("op {op}: the {role} anchor must be a `LINE#HASH` anchor, not BOF/EOF")]
    NotALineAnchor { op: usize, role: &'static str },
    #[error("op {op}: range end (line {end}) is before start (line {start})")]
    ReversedRange { op: usize, start: usize, end: usize },
    #[error("op {op}: an insert needs at least one line")]
    EmptyInsert { op: usize },
    #[error("ops {first} and {second} overlap ({detail}); merge them or use disjoint ranges")]
    Overlap {
        first: usize,
        second: usize,
        detail: String,
    },
    /// Whole edit rejected; `message` lists the stale anchors and fresh anchored lines.
    #[error("{message}")]
    Stale {
        anchors: Vec<StaleAnchor>,
        message: String,
    },
}
