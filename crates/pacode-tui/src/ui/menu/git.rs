//! The `Git` level (§5.10) — read-only commit history: one [`MenuRow::Rich`]
//! per commit carrying the lane graph, the ref badges, the subject, the age
//! and the author, with the short hash flush right. `enter` opens the
//! commit's patch in the side panel (`PanelTarget::CommitDiff`); `ctrl+b`
//! pushes the refs collected from the loaded history as a second stack level
//! (`MenuId::GitBranches` — bare `b` types into the local filter, so the
//! action rides the chord).
//!
//! The data arrives via `Reply::GitLog` (or the disk snapshot before it,
//! §8.2); `state.lists` owns the `loading…`/`offline copy` badge, while an
//! in-flight page append shows `loading more`. Nothing is fetched from the
//! view or the key handlers: the draw records [`wants`] — the selected
//! commit's `--stat` for the detail pane and the next history page when the
//! cursor rests on the last loaded row — and `dispatch_deferred` turns them
//! into `Action::GitShow` / `Action::GitLog` (§8.1).

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_types::time::{format_duration_ms, now_ms};
use pacode_types::vcs::{CommitRow, GitStatus, RefBadge};

use crate::keys::Action;
use crate::state::{AppState, Focus, PanelTarget};

use super::model::{
    Detail, DetailLine, LabelSpan, LabelTone, Mark, MenuRow, RowId, resolve_cursor,
};
use super::stack::MenuStack;
use super::view::{
    EmptyState, FilterMode, KeyBinding, MenuAction, MenuId, MenuUiState, MenuView, StatusKind,
    StatusLine,
};

#[cfg(test)]
#[path = "git_tests.rs"]
mod git_tests;

/// `ctrl+b` — push the refs submenu. Bare `b` is unreachable here: the local
/// filter line owns printable keys (§3.4), so the spec's `b` rides CONTROL.
pub(crate) const BRANCHES_KEY: KeyBinding = KeyBinding {
    code: KeyCode::Char('b'),
    mods: KeyModifiers::CONTROL,
};

/// The history rows plus the declared `ctrl+b` action — or an honest empty
/// state outside a repo.
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Git);
    view.filter = Some(FilterMode::Local);
    // `enter` does not "open" a row here — it paints the commit's patch.
    view.enter_label = Some("diff");

    view.status = if state.git.loading_more {
        Some(StatusLine {
            kind: StatusKind::Loading,
            text: "loading more".to_string(),
        })
    } else {
        match state.lists.badge(crate::cache::ListKind::GitLog) {
            Some(crate::cache::Badge::Loading) => Some(StatusLine::loading()),
            Some(crate::cache::Badge::OfflineCopy) => Some(StatusLine::offline_copy()),
            None => None,
        }
    };

    let ascii = state.config.ui.ascii_only;
    let now = now_ms();
    // One `LaneCell` per loaded row, in list order — appended pages keep
    // their lanes because `apply_log` dedupes by hash (lanes.rs assumes a
    // hash appears once).
    let cells = pacode_vcs::lanes(&state.git.commits);
    for (commit, cell) in state.git.commits.iter().zip(cells.iter()) {
        view.rows.push(commit_row(commit, cell, ascii, now));
    }

    // The `--stat` block lives only on the selected card — the deferred
    // `GitShow` fetch is armed for exactly this hash (`wants` below).
    if let Some(i) = resolve_cursor(&view.rows, &ui.cursor)
        && let Some(hash) = view.rows[i].id().and_then(commit_hash)
        && let MenuRow::Rich { detail, .. } = &mut view.rows[i]
    {
        attach_stat(state, &hash, detail);
    }

    view.actions = vec![MenuAction::new(BRANCHES_KEY, "branches")];
    // Set unconditionally: the local filter may still drain the list — a
    // dead query then says "no matches", not "no commits yet" (§3.2).
    view.empty = empty_state(state, ui);
    view
}

/// `enter` — the commit's patch in the side panel (§5.10). The menu closes
/// like a picker: `focus` moves to `Panel`, `esc` there lands on `Normal`.
/// `Action::GitDiff` flies now; `LoadPanel` skips `CommitDiff` targets.
pub fn on_row(
    state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let Some(hash) = commit_hash(row) else {
        return vec![];
    };
    // A stale patch must not paint under the new target — the panel reads
    // `state.git.diff` keyed by hash, but dropping it keeps the "loading"
    // state honest while the request is in flight.
    state.git.diff = None;
    state.panel.target = Some(PanelTarget::CommitDiff { hash: hash.clone() });
    // `panel.scroll` counts back from the tail (the log targets'
    // convention): `usize::MAX` pins the head, where a diff reads from.
    state.panel.scroll = usize::MAX;
    state.focus = Focus::Panel {
        target: PanelTarget::CommitDiff { hash: hash.clone() },
        follow: false,
        follow_paused: false,
    };
    vec![Action::GitDiff { hash }]
}

/// Declared actions: `ctrl+b` descends into the refs collected from the
/// loaded history (§5.10's `b` — see [`BRANCHES_KEY`] for the chord).
pub fn on_action(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    action: &MenuAction,
    _row: Option<&RowId>,
) -> Vec<Action> {
    if action.key != BRANCHES_KEY {
        return vec![];
    }
    crate::keys_menu::descend(state, stack, ui, MenuId::GitBranches)
}

/// What the level wants fetched for the current cursor — the draw records
/// the pair into `state.git.show_want` / `more_want`; `dispatch_deferred`
/// turns them into requests (§8.1: no fetch inside the view or a key).
///
/// `detail_wide` says the detail panel (not the two collapsed lines) is on
/// screen — the `--stat` want exists only where it can be seen. The page
/// want is the cursor's hash when it rests on the last loaded commit while
/// `has_more` holds — `after` in the next `GitLog` (§5.10).
pub fn wants(
    state: &AppState,
    view: &MenuView,
    ui: &MenuUiState,
    detail_wide: bool,
) -> (Option<String>, Option<String>) {
    let sel_hash = resolve_cursor(&view.rows, &ui.cursor)
        .and_then(|i| view.rows[i].id().cloned())
        .and_then(|id| commit_hash(&id));
    let show = if detail_wide { sel_hash.clone() } else { None };
    let more = match (&sel_hash, state.git.commits.last()) {
        (Some(hash), Some(last)) if *hash == last.hash && state.git.has_more => Some(hash.clone()),
        _ => None,
    };
    (show, more)
}

/// Refresh-on-open: a session whose `GitLog` never landed (attach raced the
/// first request, or a transport error dropped it) asks for the first page
/// when the level opens. A known status — even `NotARepo` — does not
/// refire; an in-flight request stands.
pub fn on_open(state: &AppState) -> Vec<Action> {
    if state.git.status.is_none() && !state.git.loading {
        vec![Action::GitLog { after: None }]
    } else {
        vec![]
    }
}

/// `commit:<hash>` → the hash; `None` for any other row id.
fn commit_hash(id: &RowId) -> Option<String> {
    id.as_str().strip_prefix("commit:").map(str::to_string)
}

/// One history row: `[graph] [badges] subject  age · author` with the short
/// hash flush right. The full hash is searchable through `detail.search`.
fn commit_row(commit: &CommitRow, cell: &pacode_vcs::LaneCell, ascii: bool, now: u64) -> MenuRow {
    let mut spans = Vec::new();
    let graph = cell.line(ascii);
    if !graph.is_empty() {
        spans.push(LabelSpan::new(format!("{graph} "), LabelTone::Faint));
    }
    spans.extend(ref_spans(&commit.refs, ascii));
    spans.push(LabelSpan::new(commit.subject.clone(), LabelTone::Text));
    let age = format_duration_ms(now.saturating_sub(commit.date_ms));
    spans.push(LabelSpan::new(format!("  {age}"), LabelTone::Faint));
    spans.push(LabelSpan::new(
        format!(" · {}", commit.author),
        LabelTone::Dim,
    ));
    MenuRow::Rich {
        id: RowId::new(format!("commit:{}", commit.hash)),
        mark: Mark::None,
        spans,
        right: Some(commit.short.clone()),
        detail: commit_detail(commit, now),
    }
}

/// The decorations of one commit as label spans — `HEAD → branch` merges
/// into one badge; each kind keeps its own tone (§5.10): HEAD accent, local
/// branch green, `origin/…` cyan, tag yellow.
fn ref_spans(refs: &[RefBadge], ascii: bool) -> Vec<LabelSpan> {
    let mut spans = Vec::new();
    let arrow = if ascii { "->" } else { "→" };
    let mut i = 0;
    while i < refs.len() {
        // `consumed` — 2 when HEAD folds into the ref it points at.
        let (text, tone, consumed) = match &refs[i] {
            RefBadge::Head => match refs.get(i + 1) {
                // `git log --decorate` emits HEAD plus the ref it points at.
                Some(RefBadge::Branch(name)) | Some(RefBadge::Remote(name)) => {
                    (format!("HEAD{arrow}{name}"), LabelTone::Accent, 2)
                }
                // Detached HEAD — a lone badge.
                _ => ("HEAD".to_string(), LabelTone::Accent, 1),
            },
            RefBadge::Branch(name) => (name.clone(), LabelTone::Green, 1),
            RefBadge::Remote(name) => (name.clone(), LabelTone::Cyan, 1),
            RefBadge::Tag(name) => (name.clone(), LabelTone::Yellow, 1),
        };
        if !spans.is_empty() {
            spans.push(LabelSpan::new(" ", LabelTone::Faint));
        }
        spans.push(LabelSpan::new(text, tone));
        i += consumed;
    }
    if !spans.is_empty() {
        spans.push(LabelSpan::new(" ", LabelTone::Faint));
    }
    spans
}

/// The selected commit's card: the full subject, the body, author and date —
/// the `--stat` block is attached separately once it lands (`attach_stat`).
fn commit_detail(commit: &CommitRow, now: u64) -> Detail {
    let mut lines = vec![DetailLine::Text(commit.subject.clone())];
    lines.extend(commit.body.lines().map(|l| DetailLine::Text(l.to_string())));
    lines.push(DetailLine::Field {
        label: "author".to_string(),
        value: commit.author.clone(),
    });
    lines.push(DetailLine::Field {
        label: "date".to_string(),
        value: format_date(commit.date_ms, now),
    });
    Detail {
        title: commit.short.clone(),
        lines,
        actions: Vec::new(),
        // The full hash and the author are findable through the local filter
        // even though the row shows only the short hash (§5.10).
        search: vec![commit.hash.clone(), commit.author.clone()],
    }
}

/// `2026-09-12 14:03 · 3h ago` — absolute local time plus the relative age
/// (same `chrono::Local` convention `sessions` uses). A timestamp chrono
/// cannot represent falls back to the age alone.
fn format_date(date_ms: u64, now: u64) -> String {
    let age = format!("{} ago", format_duration_ms(now.saturating_sub(date_ms)));
    let Ok(ms) = i64::try_from(date_ms) else {
        return age;
    };
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => format!(
            "{} · {age}",
            dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
        ),
        None => age,
    }
}

/// The `--stat` lines for the selected commit — the fetched block, the
/// recorded failure, or the honest "loading" line while `show_want` is in
/// flight. Everything reads `state.git.detail` — no IO in the view (§8.1).
fn attach_stat(state: &AppState, hash: &str, detail: &mut Detail) {
    match &state.git.detail {
        Some(d) if d.hash == hash => {
            if let Some(error) = &d.error {
                detail
                    .lines
                    .push(DetailLine::Text(format!("stat unavailable: {error}")));
            } else {
                match d.stat.as_deref() {
                    Some("") => detail
                        .lines
                        .push(DetailLine::Text("no file changes".to_string())),
                    Some(stat) => detail
                        .lines
                        .extend(stat.lines().map(|l| DetailLine::Text(l.to_string()))),
                    None => {}
                }
            }
        }
        // Any other hash (or nothing fetched yet) reads as loading — the
        // want for this row was recorded this frame.
        _ => detail
            .lines
            .push(DetailLine::Text("loading stat…".to_string())),
    }
}

/// What an empty list says — a real repo condition (`NotARepo`,
/// `GitMissing`), the first page in flight, a genuine empty history, or a
/// filter that hid every loaded row.
fn empty_state(state: &AppState, ui: &MenuUiState) -> EmptyState {
    if !ui.query.is_empty() {
        return EmptyState::new("no matches", "the filter hides every loaded commit");
    }
    match state.git.status {
        Some(GitStatus::NotARepo) => {
            EmptyState::new("not a git repo", "the session cwd is not a checkout")
        }
        Some(GitStatus::GitMissing) => EmptyState::new(
            "git is not installed",
            "the git binary could not be spawned",
        ),
        Some(GitStatus::Ready) | None => {
            if state.git.loading || state.lists.git.in_flight {
                EmptyState::new("loading…", "the first history page is on its way")
            } else {
                EmptyState::new(
                    "no commits yet",
                    "the history appears after the first commit",
                )
            }
        }
    }
}
