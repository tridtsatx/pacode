//! `git log` machine-format parsing.
//!
//! Format string (kept in [`LOG_FORMAT`]):
//! `%H %h %P %D %s %an %ct %b`, fields separated by `US` (`\x1f`), records
//! terminated by `RS` (`\x1e`). The body is the last field, so a stray `\x1f`
//! inside it survives a `splitn`. A `\x1e` inside a body would end the record
//! early; the malformed tail then fails field-count validation and is skipped.
//!
//! Decorations come from `--decorate=full`, so `%D` yields unambiguous
//! `refs/heads/…`, `refs/remotes/…`, `tag: refs/tags/…` names plus
//! `HEAD -> <ref>`.

use pacode_types::vcs::{CommitRow, RefBadge};

/// Byte used between fields inside one commit record.
pub(crate) const FIELD_SEP: char = '\x1f';
/// Byte terminating one commit record.
pub(crate) const RECORD_SEP: char = '\x1e';

/// `--format=` argument for `git log`: hash, short hash, parents, decorations,
/// subject, author, committer timestamp (unix seconds), body.
pub(crate) const LOG_FORMAT: &str = "%H%x1f%h%x1f%P%x1f%D%x1f%s%x1f%an%x1f%ct%x1f%b%x1e";

/// Number of fields one record must have; the last one (body) keeps any
/// embedded `FIELD_SEP`.
const FIELD_COUNT: usize = 8;

/// Parse the whole `git log` stdout into rows; malformed records are skipped.
pub(crate) fn parse_log(stdout: &[u8]) -> Vec<CommitRow> {
    let text = String::from_utf8_lossy(stdout);
    text.split(RECORD_SEP)
        .filter_map(|rec| parse_record(rec.trim_start_matches('\n')))
        .collect()
}

fn parse_record(rec: &str) -> Option<CommitRow> {
    if rec.is_empty() {
        return None;
    }
    let mut f = rec.splitn(FIELD_COUNT, FIELD_SEP);
    let hash = f.next()?.to_string();
    let short = f.next()?.to_string();
    let parents: Vec<String> = f.next()?.split_whitespace().map(str::to_string).collect();
    let refs = parse_decorations(f.next()?);
    let subject = f.next()?.to_string();
    let author = f.next()?.to_string();
    let date_ms = f.next()?.parse::<u64>().ok()?.saturating_mul(1000);
    // git leaves the body's own trailing newline on the record.
    let body_raw = f.next()?;
    let body = body_raw.strip_suffix('\n').unwrap_or(body_raw).to_string();
    if hash.is_empty() {
        return None;
    }
    Some(CommitRow {
        hash,
        short,
        parents,
        refs,
        subject,
        body,
        author,
        date_ms,
    })
}

/// Parse one `%D` decoration list: `"HEAD -> refs/heads/main, tag: refs/tags/v1"`.
fn parse_decorations(decorations: &str) -> Vec<RefBadge> {
    let mut out = Vec::new();
    for item in decorations.split(", ") {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Some(target) = item.strip_prefix("HEAD -> ") {
            out.push(RefBadge::Head);
            out.push(classify_ref(target));
        } else if item == "HEAD" {
            out.push(RefBadge::Head);
        } else {
            out.push(classify_ref(item));
        }
    }
    out
}

/// Classify one decoration name. With `--decorate=full` these are full refs;
/// short names (no `refs/` prefix) are treated as local branches.
fn classify_ref(name: &str) -> RefBadge {
    let name = name.trim();
    if let Some(tag) = name.strip_prefix("tag: ") {
        return RefBadge::Tag(
            tag.trim()
                .strip_prefix("refs/tags/")
                .unwrap_or(tag.trim())
                .to_string(),
        );
    }
    if let Some(short) = name.strip_prefix("refs/heads/") {
        return RefBadge::Branch(short.to_string());
    }
    if let Some(short) = name.strip_prefix("refs/remotes/") {
        return RefBadge::Remote(short.to_string());
    }
    if let Some(short) = name.strip_prefix("refs/tags/") {
        return RefBadge::Tag(short.to_string());
    }
    if let Some(short) = name.strip_prefix("refs/") {
        // Namespace we do not badge distinctly (stash, notes, pull refs);
        // shown as a branch-shaped badge with its short name.
        return RefBadge::Branch(short.to_string());
    }
    RefBadge::Branch(name.to_string())
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod parse_tests;
