//! Git history transport types for the VCS submenu (spec §5.10).
//!
//! Wire contracts shared by `pacode-vcs` (producer), `pacode-core` (relay) and
//! `pacode-tui` (consumer). Pure data: no IO, no tokio.

use serde::{Deserialize, Serialize};

/// One commit as the git submenu lists it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitRow {
    /// Full object hash (40 hex).
    pub hash: String,
    /// Git-abbreviated hash (`%h`, adaptive length).
    pub short: String,
    /// Parent hashes in order; empty for a root commit or a shallow boundary.
    #[serde(default)]
    pub parents: Vec<String>,
    /// Decorations pointing at this commit (`--decorate=full` parsed).
    #[serde(default)]
    pub refs: Vec<RefBadge>,
    /// `%s`: first line of the message.
    pub subject: String,
    /// `%b`: everything after the subject line, trimmed.
    #[serde(default)]
    pub body: String,
    /// `%an`: author name.
    pub author: String,
    /// Committer date (`%ct`) in milliseconds since the epoch.
    pub date_ms: u64,
}

/// A `git log` decoration classified for rendering (`HEAD → main`, branch,
/// remote-tracking ref, tag). Externally tagged: `"head"`, `{"branch":"main"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefBadge {
    /// `HEAD` itself; followed by a `Branch`/`Remote` entry when attached.
    Head,
    /// `refs/heads/<0>` — the payload is the short branch name.
    Branch(String),
    /// `refs/remotes/<0>` — e.g. `origin/main`.
    Remote(String),
    /// `refs/tags/<0>` — the payload is the tag name.
    Tag(String),
}

/// Whether the session's cwd is a usable git checkout; a normal outcome, not
/// an error, so it travels inside `Reply::GitLog` rather than `Reply::Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStatus {
    /// History (possibly empty) is available.
    Ready,
    /// `fatal: not a git repository` — the submenu shows an empty state.
    NotARepo,
    /// The `git` binary could not be spawned at all.
    GitMissing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_row_round_trip() {
        let row = CommitRow {
            hash: "a".repeat(40),
            short: "aaaaaaa".into(),
            parents: vec!["b".repeat(40)],
            refs: vec![
                RefBadge::Head,
                RefBadge::Branch("main".into()),
                RefBadge::Remote("origin/main".into()),
                RefBadge::Tag("v1.0".into()),
            ],
            subject: "feat: x".into(),
            body: "long\nbody".into(),
            author: "Ada".into(),
            date_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&row).unwrap();
        let back: CommitRow = serde_json::from_str(&json).unwrap();
        assert_eq!(back, row);
    }

    #[test]
    fn ref_badge_wire_names() {
        let json = serde_json::to_value(RefBadge::Branch("dev".into())).unwrap();
        assert_eq!(json, serde_json::json!({"branch": "dev"}));
        assert_eq!(serde_json::to_value(RefBadge::Head).unwrap(), "head");
    }

    #[test]
    fn git_status_wire_names() {
        let json = serde_json::to_value(GitStatus::NotARepo).unwrap();
        assert_eq!(json, "not_a_repo");
    }
}
