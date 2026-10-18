//! Rule file parser: markdown with a small YAML/TOML-ish frontmatter.
//!
//! ```text
//! ---
//! name: ts-no-explicit-any
//! languages: [ts, tsx]
//! match: '(?::\s*any\b)'
//! severity: block
//! ---
//! Never write `any`; use `unknown`.
//! ```
//!
//! Lines are `key: value` or `key = value`. A value wrapped in matching `'` or `"`
//! is taken literally (no escape processing, so regexes need no doubling). Lists
//! are `[a, b]`, a bare `a, b`, or `- item` lines under an empty `key:`. Full-line
//! `#` comments and unknown keys are ignored.

use std::collections::BTreeMap;

use regex::{Regex, RegexBuilder};

use crate::rule::{Mode, Origin, Rule, RuleError, Scope, Severity};

const DEFAULT_MAX_PER_TURN: u32 = 3;
const REGEX_SIZE_LIMIT: usize = 1 << 20;

enum Field {
    Scalar(String),
    List(Vec<String>),
}

impl Field {
    fn scalar(&self) -> String {
        match self {
            Field::Scalar(s) => s.clone(),
            Field::List(l) => l.join(", "),
        }
    }

    fn list(&self) -> Vec<String> {
        match self {
            Field::Scalar(s) => split_list(s),
            Field::List(l) => l.clone(),
        }
    }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'\'' || b[0] == b'"') && b[b.len() - 1] == b[0] {
        return v[1..v.len() - 1].to_string();
    }
    v.to_string()
}

fn split_list(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = v
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(v);
    inner
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split `---\nfront\n---\nbody`. `None` when the frontmatter is absent/unterminated.
fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    let s = source.trim_start_matches('\u{feff}');
    let rest = s.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn parse_fields(front: &str) -> BTreeMap<String, Field> {
    let mut map = BTreeMap::new();
    let mut open_list: Option<String> = None;
    for raw in front.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let (Some(key), Some(item)) = (open_list.as_ref(), trimmed.strip_prefix("- "))
            && let Some(Field::List(items)) = map.get_mut(key)
        {
            items.push(unquote(item));
            continue;
        }
        open_list = None;
        let key_len = trimmed
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(trimmed.len());
        if key_len == 0 {
            continue;
        }
        let rest = trimmed[key_len..].trim_start();
        let Some(value) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('=')) else {
            continue;
        };
        let key = trimmed[..key_len].to_ascii_lowercase().replace('-', "_");
        let value = value.trim();
        if value.is_empty() {
            open_list = Some(key.clone());
            map.insert(key, Field::List(Vec::new()));
        } else if value.starts_with('[') && value.ends_with(']') {
            map.insert(key, Field::List(split_list(value)));
        } else {
            map.insert(key, Field::Scalar(unquote(value)));
        }
    }
    map
}

fn compile(origin: &str, field: &'static str, pattern: &str) -> Result<Regex, RuleError> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|source| RuleError::BadRegex {
            origin: origin.to_string(),
            field,
            source,
        })
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
}

/// Parse one rule file. Never panics; every problem is a [`RuleError`].
pub fn parse_rule(source: &str, origin: Origin) -> Result<Rule, RuleError> {
    let label = origin.label();
    let bad = |field: &'static str, value: &str| RuleError::BadValue {
        origin: label.clone(),
        field,
        value: value.to_string(),
    };
    let missing = |field: &'static str| RuleError::MissingField {
        origin: label.clone(),
        field,
    };

    let (front, body) = split_frontmatter(source).ok_or_else(|| RuleError::Frontmatter {
        origin: label.clone(),
    })?;
    let fields = parse_fields(front);

    let name = fields
        .get("name")
        .map(Field::scalar)
        .ok_or_else(|| missing("name"))?;
    if !valid_name(&name) {
        return Err(bad("name", &name));
    }
    let languages: Vec<String> = fields
        .get("languages")
        .map(Field::list)
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.trim_start_matches('.').to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    if languages.is_empty() {
        return Err(missing("languages"));
    }
    let pattern_src = fields
        .get("match")
        .map(Field::scalar)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| missing("match"))?;
    let pattern = compile(&label, "match", &pattern_src)?;
    let unless = match fields
        .get("unless")
        .map(Field::scalar)
        .filter(|s| !s.is_empty())
    {
        Some(src) => Some(compile(&label, "unless", &src)?),
        None => None,
    };
    let unless_above = match fields
        .get("unless_above")
        .map(Field::scalar)
        .filter(|s| !s.is_empty())
    {
        Some(src) => Some(compile(&label, "unless_above", &src)?),
        None => None,
    };

    let scope = match fields
        .get("scope")
        .map(|f| f.scalar().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("code") => Scope::Code,
        Some("any") => Scope::Any,
        Some(other) => return Err(bad("scope", other)),
    };
    let severity = match fields
        .get("severity")
        .map(|f| f.scalar().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("warn") => Severity::Warn,
        Some("block") => Severity::Block,
        Some(other) => return Err(bad("severity", other)),
    };
    let mode = match fields
        .get("mode")
        .map(|f| f.scalar().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("after_write") | Some("after-write") => Mode::AfterWrite,
        Some("stream") => Mode::Stream,
        Some(other) => return Err(bad("mode", other)),
    };
    let max_per_turn = match fields.get("max_per_turn").map(Field::scalar) {
        None => DEFAULT_MAX_PER_TURN,
        Some(v) => match v.parse::<u32>() {
            Ok(n) if n >= 1 => n,
            _ => return Err(bad("max_per_turn", &v)),
        },
    };
    let exclude = fields.get("exclude").map(Field::list).unwrap_or_default();
    let skip_test_code = match fields
        .get("skip_test_code")
        .map(|f| f.scalar().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => return Err(bad("skip_test_code", other)),
    };

    let message = body.trim().to_string();
    if message.is_empty() {
        return Err(missing("body"));
    }

    Ok(Rule {
        name,
        languages,
        pattern,
        unless,
        unless_above,
        scope,
        severity,
        mode,
        max_per_turn,
        exclude,
        skip_test_code,
        message,
        origin,
    })
}
