//! The `Sessions` level (§5.6 — variant A+B): the resumable session list
//! grouped by recency — `today` / `this week` / `earlier` on the session's
//! `updated_at` — with a detail card per row. `enter` resumes through
//! `Attach::Resume`.
//!
//! Spec gap: `SessionMeta` carries no turn count, context size, cost or
//! last-reply text — the card shows what the protocol sends today (§5.6
//! lists more than the wire has), and the first prompt fills the preview
//! slot, capped at three lines cut on char boundaries.

use chrono::{DateTime, Local, TimeZone};
use pacode_types::time::{format_duration_ms, now_ms};
use pacode_types::{Attach, Request, SessionId, SessionMeta};

use crate::keys::Action;
use crate::state::AppState;
use crate::ui::overlays::{age_ago, short_path};

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{EmptyState, FilterMode, MenuId, MenuUiState, MenuView, StatusLine};

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod sessions_tests;

const DAY_MS: u64 = 86_400_000;
/// `this week` reaches this far back from the start of today — both bounds
/// are day-anchored, so a session never hops groups in the middle of a day.
const WEEK_MS: u64 = 7 * DAY_MS;
/// Lines of the prompt preview a detail card may show (§5.6: the reply is
/// capped at three lines).
const PREVIEW_MAX_LINES: usize = 3;
/// Characters a single preview line may hold — long first lines are cut on
/// a char boundary, not a byte one (§5.6).
const PREVIEW_LINE_CHARS: usize = 160;
/// The open hook's list size — the legacy picker's `ListSessions` limit.
const LIST_LIMIT: u32 = 50;

/// The recency bucket of one session, given the local-midnight boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    /// `updated` since the start of today.
    Today,
    /// The seven days before today.
    Week,
    /// Anything older.
    Earlier,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Week => "this week",
            Self::Earlier => "earlier",
        }
    }
}

fn group_of(updated_ms: u64, day_start_ms: u64) -> Group {
    if updated_ms >= day_start_ms {
        Group::Today
    } else if updated_ms >= day_start_ms.saturating_sub(WEEK_MS) {
        Group::Week
    } else {
        Group::Earlier
    }
}

/// Local midnight that starts the day containing `now_ms`, as epoch ms —
/// the user's wall clock, same convention as the `stats` lines. UTC
/// midnight is the fallback when the conversion cannot resolve.
fn day_start(now_ms: u64) -> u64 {
    DateTime::from_timestamp_millis(now_ms as i64)
        .and_then(|dt| {
            let date = dt.with_timezone(&Local).date_naive();
            Local
                .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
                .earliest()
        })
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
        .unwrap_or(now_ms - now_ms % DAY_MS)
}

/// The view — rebuilt per frame from `state.sessions` (§8.2 seeds it from
/// the disk cache before the first frame; `lists` owns the badge).
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Sessions);
    view.filter = Some(FilterMode::Local);
    view.status = match state.lists.badge(crate::cache::ListKind::Sessions) {
        Some(crate::cache::Badge::Loading) => Some(StatusLine::loading()),
        Some(crate::cache::Badge::OfflineCopy) => Some(StatusLine::offline_copy()),
        None => None,
    };

    let now = now_ms();
    let today_start = day_start(now);
    // The daemon answers `updated_at DESC`; the sort keeps that true for a
    // list that arrived scrambled (a stale or hand-written cache file).
    let mut sorted: Vec<&SessionMeta> = state.sessions.iter().collect();
    sorted.sort_by_key(|meta| std::cmp::Reverse(meta.updated_at_ms));

    let mut grouped: [Vec<MenuRow>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for s in sorted {
        let bucket = match group_of(s.updated_at_ms, today_start) {
            Group::Today => &mut grouped[0],
            Group::Week => &mut grouped[1],
            Group::Earlier => &mut grouped[2],
        };
        bucket.push(session_row(state, s, now));
    }
    let mut rows = Vec::new();
    for (group, bucket) in [Group::Today, Group::Week, Group::Earlier]
        .into_iter()
        .zip(grouped)
    {
        if bucket.is_empty() {
            continue;
        }
        rows.push(MenuRow::section(group.label()));
        rows.extend(bucket);
    }
    view.rows = rows;

    view.empty = if state.sessions.is_empty() && state.lists.sessions.in_flight {
        EmptyState::new("loading…", "the session list is on its way")
    } else if ui.query.is_empty() {
        EmptyState::new("no sessions yet", "this is the first one")
    } else {
        EmptyState::new("no sessions match", "backspace to clear the filter")
    };
    view
}

/// `enter` — resume the session (§5.6): the stack drains so the menu
/// closes like the legacy picker did, then `Attach::Resume` goes out.
pub fn on_row(
    state: &mut AppState,
    stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    // The row came from this list a frame ago; a refresh may have dropped
    // it — resuming an id the daemon no longer lists is not a thing.
    if !state.sessions.iter().any(|s| s.id.as_str() == row.as_str()) {
        return vec![];
    }
    log::debug!("close menu: Sessions → resume {row}");
    while stack.pop().is_some() {}
    // Already on screen: re-attaching would only flash "connecting" and
    // rebuild what is already here.
    if state
        .meta
        .as_ref()
        .is_some_and(|meta| meta.id.as_str() == row.as_str())
    {
        return vec![];
    }
    let attach = Attach::Resume {
        session: SessionId::new(row.as_str()),
    };
    state.leave_active_slot();
    state.begin_attach(attach.clone());
    vec![Action::SwitchSession { attach }]
}

/// Refresh-on-open — the `ListSessions` the legacy picker sent; `lists`
/// turns the in-flight flag into the `loading…` badge (§8.2).
pub fn on_open(_state: &AppState) -> Vec<Action> {
    vec![Action::Send(Request::ListSessions { limit: LIST_LIMIT })]
}

/// One row: title on the left, `branch · age` on the right (§5.6). The
/// currently attached session wears the live mark.
fn session_row(state: &AppState, s: &SessionMeta, now: u64) -> MenuRow {
    let age = age_ago(s.updated_at_ms, now);
    let right = match &s.git_branch {
        Some(branch) => format!("{branch} · {age}"),
        None => age,
    };
    let current = state
        .meta
        .as_ref()
        .is_some_and(|m| m.id.as_str() == s.id.as_str());
    MenuRow::Item {
        id: RowId::new(s.id.as_str()),
        mark: if current { Mark::Active } else { Mark::None },
        label: s.title(),
        right: Some(right),
        detail: session_detail(s),
    }
}

/// The detail card (§5.6): id, path and branch, the session's lifetime as
/// its duration, model and mode, then the first prompt as the preview —
/// capped at [`PREVIEW_MAX_LINES`] lines, each cut on a char boundary.
fn session_detail(s: &SessionMeta) -> Detail {
    let mut lines = vec![
        field("id", s.id.as_str()),
        field("path", short_path(&s.cwd)),
    ];
    if let Some(branch) = &s.git_branch {
        lines.push(field("branch", branch));
    }
    lines.push(field(
        "duration",
        format_duration_ms(s.updated_at_ms.saturating_sub(s.created_at_ms)),
    ));
    let model = s.model.to_string();
    let effort = s.effort.as_str();
    lines.push(field("model", format!("{model} · {effort}")));
    lines.push(field("mode", s.mode.label()));
    if s.zdr {
        lines.push(field("zdr", "on"));
    }
    if let Some(prompt) = &s.first_prompt {
        let mut preview = cap_lines(prompt, PREVIEW_MAX_LINES).into_iter();
        if let Some(first) = preview.next() {
            lines.push(field("first prompt", first));
            lines.extend(preview.map(DetailLine::Text));
        }
    }
    Detail {
        title: s.title(),
        lines,
        actions: Vec::new(),
        // The local filter finds a session by its id without the row
        // having to show it (§5.6: "by title and id").
        search: vec![s.id.as_str().to_string()],
    }
}

fn field(label: &str, value: impl Into<String>) -> DetailLine {
    DetailLine::Field {
        label: label.to_string(),
        value: value.into(),
    }
}

/// At most `max` lines of `text`: its own line breaks are kept, each line
/// is capped at [`PREVIEW_LINE_CHARS`] chars (`.chars()` — the cut lands on
/// a boundary, never mid-UTF-8), and a trailing `…` marks either cut.
fn cap_lines(text: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cut_off = false;
    for line in text.lines() {
        if out.len() == max {
            cut_off = true;
            break;
        }
        let shown: String = line.chars().take(PREVIEW_LINE_CHARS).collect();
        if shown.len() < line.len() {
            out.push(format!("{shown}…"));
        } else {
            out.push(shown);
        }
    }
    if cut_off
        && let Some(last) = out.last_mut()
        && !last.ends_with('…')
    {
        last.push('…');
    }
    out
}
