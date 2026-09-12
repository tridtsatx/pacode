//! The `Files` level (§5.5): files the turn's tools touched, grouped by
//! directory — a `Section` row per directory carrying the `N · +A −R`
//! aggregate, files under it named relative to it. Sections are headers, so
//! the cursor can never rest on one.
//!
//! `enter` opens the file's worktree diff in the side panel
//! (`PanelTarget::FileDiff`); `o` opens it in the external editor. The detail
//! card shows the lexed head of the file out of `files.cached_preview` — the
//! read + lex ran in the deferred preview job (`files.preview_want`), never in
//! the draw path (§8.1). Row labels `new`/`deleted`/`renamed` come from the
//! deferred `git status` scan (`files.statuses`); the `+A −R` numbers from the
//! transcript's `DiffStat`s (`files.apply_diff`).

use std::collections::BTreeMap;
use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_types::time::{format_duration_ms, now_ms};

use crate::keys::Action;
use crate::state::files::{
    FileKinds, FileRow, FileStatus, PREVIEW_BYTES, PREVIEW_LINES, PreviewKey, PreviewKind,
    PreviewLine, TextPreview,
};
use crate::state::{AppState, Focus, PanelTarget};

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId, resolve_cursor};
use super::stack::MenuStack;
use super::view::{EmptyState, KeyBinding, MenuAction, MenuId, MenuUiState, MenuView};

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;

/// `o` — open in the external editor (§5.5). Bare char: the level carries no
/// filter line, so printable keys reach declared actions.
const OPEN_KEY: KeyBinding = KeyBinding {
    code: KeyCode::Char('o'),
    mods: KeyModifiers::NONE,
};

/// The Files view — rebuilt per frame from live state.
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Files);

    for (dir, files) in group_by_dir(&state.files.rows) {
        view.rows.push(MenuRow::Section {
            label: dir_label(&dir),
            right: Some(aggregate_text(&files)),
        });
        for row in files {
            view.rows.push(file_row(state, row));
        }
    }

    // The preview lines live only on the selected card: the cache holds a
    // single entry and the deferred probe is armed for the selection anyway.
    if let Some(sel) = resolve_cursor(&view.rows, &ui.cursor)
        && let MenuRow::Item { id, detail, .. } = &mut view.rows[sel]
    {
        attach_preview(state, id.as_str(), detail);
    }

    view.actions = vec![MenuAction::new(OPEN_KEY, "editor").enabled(!view.rows.is_empty())];
    view.empty = EmptyState::new(
        "no files touched yet",
        "files the agent reads or writes land here",
    );
    view
}

/// `enter` — the file's worktree diff in the side panel (§5.5, §7). The menu
/// closes like a picker: `focus` moves to `Panel`, and `esc` there lands on
/// `Normal`. The patch is armed through `files.diff_want` — the event loop's
/// deferred `file_diff` job, never a git call inside a key handler (§8.1).
pub fn on_row(
    state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let path = row.as_str().to_string();
    state.files.diff_want = Some(path.clone());
    state.files.preview_want = None;
    state.panel.target = Some(PanelTarget::FileDiff { path: path.clone() });
    state.panel.diff = None;
    // `panel.scroll` counts back from the tail (the log targets'
    // convention): `usize::MAX` pins the head, where a diff reads from.
    state.panel.scroll = usize::MAX;
    state.focus = Focus::Panel {
        target: PanelTarget::FileDiff { path },
        follow: false,
        follow_paused: false,
    };
    vec![]
}

/// `o` — open the file in the external editor (§5.5): resolved against the
/// session cwd and run through `editor::open_path`, which suspends the TUI.
pub fn on_action(
    state: &mut AppState,
    _ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    if action.key != OPEN_KEY {
        return vec![];
    }
    let Some(row) = row else {
        return vec![];
    };
    // `Path::join` keeps absolute row paths as they are.
    let path = state.cwd().join(row.as_str());
    crate::editor::open_path(state, &path);
    vec![]
}

/// What the detail card needs for the current selection — recorded by the
/// draw path into `files.preview_want`; `None` while the cursor rests on
/// nothing (empty or fully filtered list).
pub fn preview_want(view: &MenuView, ui: &MenuUiState) -> Option<PreviewKey> {
    let sel = resolve_cursor(&view.rows, &ui.cursor)?;
    let id = view.rows[sel].id()?;
    Some(PreviewKey {
        raw: id.as_str().to_string(),
        kind: PreviewKind::Text,
    })
}

/// The deferred preview job's text probe: read the head (≤
/// [`PREVIEW_BYTES`]), skip images and binaries, lex each kept line so the
/// detail card only paints tokens. Runs on a blocking thread — this is the
/// *job* half of the §8.1 split; the view side is [`attach_preview`].
pub(crate) fn text_preview(path: &Path) -> Option<TextPreview> {
    use std::io::Read;
    let shown = path.to_string_lossy();
    if crate::ui::files::is_image_file(&shown) {
        return None;
    }
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(PREVIEW_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    // A NUL in the head marks a binary — the card reports "no preview"
    // rather than painting mojibake.
    if buf.contains(&0) {
        return None;
    }
    let cut_bytes = buf.len() > PREVIEW_BYTES;
    buf.truncate(PREVIEW_BYTES);
    // `from_utf8_lossy` absorbs a multi-byte char cut at the byte cap.
    let text = String::from_utf8_lossy(&buf);
    let mut lexer = pacode_render::syntax::Lexer::detect(&shown);
    let mut lines = Vec::new();
    let mut cut_lines = false;
    for line in text.lines() {
        if lines.len() >= PREVIEW_LINES {
            cut_lines = true;
            break;
        }
        let toks = lexer
            .as_mut()
            .map(|lx| lx.lex_line(line))
            .unwrap_or_default();
        lines.push(PreviewLine {
            text: line.to_string(),
            toks,
        });
    }
    Some(TextPreview {
        lines,
        truncated: cut_bytes || cut_lines,
    })
}

/// Rows grouped under their parent directory — the tree is one level of
/// distinct parents (§5.5). Dirs sort by path, files inside by name.
fn group_by_dir(rows: &[FileRow]) -> Vec<(String, Vec<&FileRow>)> {
    let mut map: BTreeMap<String, Vec<&FileRow>> = BTreeMap::new();
    for row in rows {
        map.entry(parent_dir(&row.path)).or_default().push(row);
    }
    for files in map.values_mut() {
        files.sort_by(|a, b| {
            file_name(&a.path)
                .cmp(file_name(&b.path))
                .then_with(|| a.path.cmp(&b.path))
        });
    }
    map.into_iter().collect()
}

/// The row's parent directory; `""` for cwd-level files (labelled `.`).
fn parent_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The name relative to its directory section — the common prefix is the
/// section's own label, so it is never repeated in the item (§5.5).
fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(path)
}

fn dir_label(dir: &str) -> String {
    if dir.is_empty() {
        ".".to_string()
    } else {
        dir.to_string()
    }
}

/// `N` while nothing in the directory carries diff numbers — `+0 −0` would
/// claim a zero diff where there is simply no data — else `N · +A −R` (§5.5).
fn aggregate_text(files: &[&FileRow]) -> String {
    let n = files.len();
    let added: u32 = files.iter().map(|r| r.added).sum();
    let removed: u32 = files.iter().map(|r| r.removed).sum();
    if added + removed == 0 {
        format!("{n}")
    } else {
        format!("{n} · +{added} −{removed}")
    }
}

/// The row's effective status: `git status` wins where it spoke; the
/// `created` heuristic (first write diff was all additions) stands in for
/// `new` until the scan lands.
fn effective_status(row: &FileRow, status: Option<FileStatus>) -> Option<FileStatus> {
    status.or(if row.created {
        Some(FileStatus::New)
    } else {
        None
    })
}

fn file_row(state: &AppState, row: &FileRow) -> MenuRow {
    let status = effective_status(row, state.files.status_of(&row.path));
    MenuRow::Item {
        id: RowId::new(row.path.clone()),
        mark: match status {
            Some(FileStatus::Deleted) => Mark::Stopped,
            Some(FileStatus::New)
            | Some(FileStatus::Renamed)
            | Some(FileStatus::Modified)
            | None => Mark::None,
        },
        label: file_name(&row.path).to_string(),
        right: right_text(row, status),
        detail: file_detail(row, status),
    }
}

/// The right cell: the status word where numbers are meaningless, else the
/// `+A −R` tail, else the op letters the legacy overlay showed as badges.
fn right_text(row: &FileRow, status: Option<FileStatus>) -> Option<String> {
    let stats = row
        .has_diff
        .then(|| format!("+{} −{}", row.added, row.removed));
    match status {
        Some(FileStatus::Deleted) => Some("deleted".to_string()),
        Some(FileStatus::Renamed) => Some(match stats {
            Some(s) => format!("renamed · {s}"),
            None => "renamed".to_string(),
        }),
        Some(FileStatus::New) => Some(match stats {
            Some(s) => format!("new · {s}"),
            None => "new".to_string(),
        }),
        Some(FileStatus::Modified) => Some(stats.unwrap_or_else(|| "modified".to_string())),
        None => stats.or_else(|| Some(ops_letters(row.kinds))),
    }
}

/// `R W E S` — the compact op letters for a row with no diff numbers.
fn ops_letters(kinds: FileKinds) -> String {
    let mut s = String::new();
    for (flag, letter) in [
        (FileKinds::READ, "R"),
        (FileKinds::WRITE, "W"),
        (FileKinds::EDIT, "E"),
        (FileKinds::SEARCH, "S"),
    ] {
        if kinds.contains(flag) {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(letter);
        }
    }
    s
}

fn file_detail(row: &FileRow, status: Option<FileStatus>) -> Detail {
    let mut lines = vec![
        DetailLine::Field {
            label: "status".to_string(),
            value: status_word(status).to_string(),
        },
        DetailLine::Field {
            label: "ops".to_string(),
            value: ops_words(row.kinds),
        },
        DetailLine::Field {
            label: "touches".to_string(),
            value: format!("{}", row.count),
        },
        DetailLine::Field {
            label: "last touch".to_string(),
            value: format!(
                "{} ago",
                format_duration_ms(now_ms().saturating_sub(row.last_ts_ms))
            ),
        },
    ];
    if row.has_diff {
        lines.push(DetailLine::Field {
            label: "diff".to_string(),
            value: format!("+{} −{}", row.added, row.removed),
        });
    }
    Detail {
        title: row.path.clone(),
        lines,
        actions: vec![MenuAction::new(OPEN_KEY, "editor")],
        // The full path is searchable even though the label drops the prefix.
        search: vec![row.path.clone()],
    }
}

fn status_word(status: Option<FileStatus>) -> &'static str {
    match status {
        Some(FileStatus::New) => "new",
        Some(FileStatus::Deleted) => "deleted",
        Some(FileStatus::Renamed) => "renamed",
        Some(FileStatus::Modified) => "modified",
        None => "touched",
    }
}

/// `read · write` — the long form for the detail card.
fn ops_words(kinds: FileKinds) -> String {
    let mut words = Vec::new();
    if kinds.has_read() {
        words.push("read");
    }
    if kinds.has_write() {
        words.push("write");
    }
    if kinds.has_edit() {
        words.push("edit");
    }
    if kinds.has_search() {
        words.push("search");
    }
    words.join(" · ")
}

/// Append the preview block to the selected row's card: the lexed head when
/// the deferred job has landed, an honest placeholder otherwise. Everything
/// here reads `files.cached_preview` — no FS, no lexing in the view (§8.1).
fn attach_preview(state: &AppState, path: &str, detail: &mut Detail) {
    let want = PreviewKey {
        raw: path.to_string(),
        kind: PreviewKind::Text,
    };
    match &state.files.cached_preview {
        Some(c) if c.key == want => match &c.text {
            Some(t) if t.lines.is_empty() => {
                detail
                    .lines
                    .push(DetailLine::Text("(empty file)".to_string()));
            }
            Some(t) => {
                for line in &t.lines {
                    detail.lines.push(DetailLine::Code {
                        text: line.text.clone(),
                        toks: line.toks.clone(),
                    });
                }
                if t.truncated {
                    detail
                        .lines
                        .push(DetailLine::Text("… truncated".to_string()));
                }
            }
            None => {
                detail.lines.push(DetailLine::Text(
                    "no preview — image, binary, or unreadable".to_string(),
                ));
            }
        },
        _ => {
            detail
                .lines
                .push(DetailLine::Text("loading preview…".to_string()));
        }
    }
}
