//! Public value types of the ledger state: who wrote, what drifted, the verdict of a
//! check and the digest rows. Pure data; re-exported by `state`.

use std::path::PathBuf;

use pacode_types::AgentId;

use super::ranges::LineRanges;
use super::snapshot::Change;

/// Who produced a version of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writer {
    Agent {
        id: AgentId,
        name: String,
    },
    /// Not written through pacode's tools (user editor, another program).
    External,
}

impl Writer {
    pub fn agent(id: &AgentId, name: &str) -> Self {
        Self::Agent {
            id: id.clone(),
            name: name.to_string(),
        }
    }

    /// `agent reviewer`, `agent main (you)` or `external (...)`.
    pub fn describe(&self, viewer: &AgentId) -> String {
        match self {
            Self::Agent { id, name } if id == viewer => format!("agent {name} (you)"),
            Self::Agent { name, .. } => format!("agent {name}"),
            Self::External => "external (an editor or a process outside pacode)".to_string(),
        }
    }
}

/// What changed since an agent's read that it has not re-read: the writers since its
/// version and the changed regions (`new` side = current line numbers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    pub writers: Vec<Writer>,
    pub changes: Vec<Change>,
    /// Lines in the current version.
    pub line_count: usize,
    /// One side is a large-file snapshot without per-line hashes (see
    /// [`super::snapshot::MAX_LINE_SNAPSHOT_BYTES`]): `changes` is the whole file and
    /// the messages say "the file changed" instead of naming lines.
    pub coarse: bool,
}

/// Outcome of [`LedgerState::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The file is new, or nothing changed since the agent's read.
    Clean,
    /// The file changed since the agent's read, but not on lines this write touches.
    Disjoint(Drift),
    /// The file exists and the agent has no seen state for it.
    NotRead,
    /// Changes the agent has not seen overlap the write (or the edit failed).
    Conflict(Drift),
}

/// One row of an agent's digest (compaction pin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestRow {
    pub path: PathBuf,
    pub version: String,
    pub wrote: bool,
    pub read: LineRanges,
    pub drift: Option<Drift>,
}
