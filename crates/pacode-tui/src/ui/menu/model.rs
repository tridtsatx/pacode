//! Row-domain data model for the shared centred-submenu base (design spec
//! §3.1): [`MenuRow`], [`RowId`], [`Mark`], [`ValueKind`], [`Detail`].
//!
//! A submenu does not draw itself: it builds a [`MenuView`](super::MenuView)
//! of typed rows and declared actions on every frame, and the base turns that
//! into the overlay. Rows are built per frame and die with it — there is no
//! row cache.

use std::fmt;

use super::view::{ActionChip, MenuId};

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;

/// Identity of one selectable row. The cursor holds a `RowId`, not an index,
/// so the selection survives re-sorts and rows arriving from the daemon (§5.9).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowId(pub String);

impl RowId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An empty id means "no remembered cursor" — it resolves to the first
    /// selectable row.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for RowId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for RowId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Status glyph at the left edge of an `Item` row. Colour is never the only
/// carrier: every mark is a distinct glyph with an ASCII fallback through the
/// glyph table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    /// No glyph — the label starts right after the pointer column.
    #[default]
    None,
    /// ● — live and ready (`main_dot`, green).
    Active,
    /// ✓ — done / loaded (`ok`, green).
    Ok,
    /// ✗ — failed (`fail`, red).
    Fail,
    /// ○ — present but not live (`agent_dot`, dim).
    Idle,
    /// ⊘ — switched off (`disabled`, dim).
    Disabled,
    /// ◍ — in flight (`running`, violet).
    Running,
    /// ◍ — starting / pending (`running`, yellow).
    Pending,
    /// ■ — stopped (`stopped`, dim).
    Stopped,
}

/// The editing contract of a `Value` row (§3.1: enum/int/float/string).
#[derive(Clone, Debug, PartialEq)]
pub enum ValueKind {
    /// `enter` cycles the options; there is no free-text edit.
    Enum {
        options: Vec<String>,
    },
    Int,
    Float,
    Text,
}

/// One line of a detail card: a field–value pair or free text (§3.1).
#[derive(Clone, Debug, PartialEq)]
pub enum DetailLine {
    /// Rendered as `label: value` — the label dim, the value plain.
    Field { label: String, value: String },
    /// A free line of text.
    Text(String),
}

/// The detail card of a row: a title, lines and the chips available for
/// exactly this row (§3.1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub title: String,
    pub lines: Vec<DetailLine>,
    pub actions: Vec<ActionChip>,
}

impl Detail {
    /// Nothing worth drawing: no title, no lines, no chips.
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.lines.is_empty() && self.actions.is_empty()
    }

    /// Lines the collapsed detail may occupy under the selected row, before
    /// the two-line cap (§3.2): the title counts as a line, chips do not.
    pub(crate) fn collapsed_lines(&self) -> usize {
        usize::from(!self.title.is_empty()) + self.lines.len()
    }
}

/// One row of the list zone (§3.1). `Section` is a header: the cursor never
/// selects it and it does not count as an item (§3.3).
#[derive(Clone, Debug, PartialEq)]
pub enum MenuRow {
    /// Group header: `[provider]`, `— today —`.
    Section {
        label: String,
        right: Option<String>,
    },
    /// Plain element: status mark, title, tail on the right.
    Item {
        id: RowId,
        mark: Mark,
        label: String,
        right: Option<String>,
        detail: Detail,
    },
    /// A bool setting; `enter` toggles. `modified` marks a value overridden
    /// away from default — rendered `*` in cyan (§5.1).
    Toggle {
        id: RowId,
        label: String,
        on: bool,
        modified: bool,
        detail: Detail,
    },
    /// A value edited in place (enum/int/float/string). `modified` as above.
    Value {
        id: RowId,
        label: String,
        value: String,
        kind: ValueKind,
        modified: bool,
        detail: Detail,
    },
    /// Enters another submenu: `enter` pushes a level onto the stack (§3.5).
    Submenu {
        id: RowId,
        label: String,
        right: Option<String>,
        target: MenuId,
    },
    /// A multi-select element (Import only, §5.7).
    Check {
        id: RowId,
        label: String,
        checked: bool,
        warn: Option<String>,
        detail: Detail,
    },
}

impl MenuRow {
    /// A section header with no right-hand tail.
    pub fn section(label: impl Into<String>) -> Self {
        Self::Section {
            label: label.into(),
            right: None,
        }
    }

    /// The selectable row id; `None` for sections.
    pub fn id(&self) -> Option<&RowId> {
        match self {
            Self::Section { .. } => None,
            Self::Item { id, .. }
            | Self::Toggle { id, .. }
            | Self::Value { id, .. }
            | Self::Submenu { id, .. }
            | Self::Check { id, .. } => Some(id),
        }
    }

    /// The detail card carried by the row; `None` for sections and submenu
    /// entries.
    pub fn detail(&self) -> Option<&Detail> {
        match self {
            Self::Item { detail, .. }
            | Self::Toggle { detail, .. }
            | Self::Value { detail, .. }
            | Self::Check { detail, .. } => Some(detail),
            Self::Section { .. } | Self::Submenu { .. } => None,
        }
    }

    /// Whether the cursor can rest on this row — sections cannot (§3.3).
    pub fn is_selectable(&self) -> bool {
        self.id().is_some()
    }
}

/// `(row index, id)` of every selectable row in order — the cursor domain.
pub fn selectable(rows: &[MenuRow]) -> impl Iterator<Item = (usize, RowId)> + '_ {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| r.id().cloned().map(|id| (i, id)))
}

/// The row the cursor rests on: the row carrying `cursor`, or the first
/// selectable row when the id is empty or went away (§5.9). `None` when the
/// list has no selectable rows at all.
pub fn resolve_cursor(rows: &[MenuRow], cursor: &RowId) -> Option<usize> {
    if !cursor.is_empty()
        && let Some(i) = rows.iter().position(|r| r.id() == Some(cursor))
    {
        return Some(i);
    }
    selectable(rows).next().map(|(i, _)| i)
}
