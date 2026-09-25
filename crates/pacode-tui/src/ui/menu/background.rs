//! The `Background` level (§5.9): one `Item` per background task — a status
//! mark, the command label, and a right-hand tail with the outcome word or,
//! while the task runs and a percent is known, the `▰▰▰▱▱▱` mini-bar.
//!
//! Rows are keyed by the daemon's task id, never by index: `TaskAdded`/
//! `TaskUpdated` mutate the list under the cursor and `k` must kill what is
//! selected, so [`RowId`] carries `TaskId` verbatim and `resolve_cursor`
//! re-finds it every frame. The redraws those events trigger are the only
//! refresh the level gets — no timer.
//!
//! The detail card's output tail is a §8.1 split: frame preparation records
//! [`tail_want`], `dispatch_deferred` runs [`read_tail`] on a blocking
//! thread, and `view` only paints the `rail.task_tail` cache. `enter` opens
//! the task's output in the side panel (`PanelTarget::Task`); `k` sends
//! `KillTask`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_types::state::{TaskInfo, TaskStatus};
use pacode_types::time::{format_duration_ms, now_ms};
use pacode_types::{Request, TaskId};

use crate::keys::Action;
use crate::state::rail::TaskTailKey;
use crate::state::{AppState, Focus, PanelTarget};

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId, resolve_cursor};
use super::stack::MenuStack;
use super::view::{EmptyState, KeyBinding, MenuAction, MenuId, MenuUiState, MenuView};

#[cfg(test)]
#[path = "background_tests.rs"]
mod background_tests;

/// `k` — kill the selected task (§5.9). Bare char: the level has no filter
/// line, so printable keys reach declared actions — and a declared action
/// outranks the `k`-as-up movement alias (§3.4); arrows and `j` still move.
const KILL_KEY: KeyBinding = KeyBinding {
    code: KeyCode::Char('k'),
    mods: KeyModifiers::NONE,
};

/// Bytes of spool output the detail card's tail holds — §5.9's 2 KiB cap,
/// always the file's end, never its head.
pub(crate) const TAIL_CAP_BYTES: u64 = 2 * 1024;
/// Tail lines the card keeps at most — the detail panel has no scroll, so a
/// longer tail is cut from the top and a marker says how much went.
const TAIL_MAX_LINES: usize = 40;
/// Cells in the `▰▰▰▱▱▱` mini-bar (§5.9's example shows six).
const BAR_CELLS: usize = 6;

/// The Background view — rebuilt per frame from live state.
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Background);
    let now = now_ms();
    let glyphs = pacode_render::Glyphs::new(state.config.ui.ascii_only);
    view.rows = state
        .rail
        .background_tasks()
        .map(|t| task_row(t, now, &glyphs))
        .collect();

    let sel = resolve_cursor(&view.rows, &ui.cursor);
    let sel_task = sel
        .and_then(|i| view.rows[i].id())
        .and_then(|id| state.rail.task(&TaskId::new(id.as_str())));
    // `k` is offered only while a live task is selected — killing a
    // terminal one is a daemon no-op the hint should not advertise (and
    // the alias gate lets `k` fall back to cursor-up on such a row).
    let killable = sel_task.is_some_and(|t| !t.status.is_terminal());

    // The tail lives only on the selected card: `rail.task_tail` holds one
    // task's tail and the deferred job is armed for exactly that id.
    if let (Some(sel), Some(task)) = (sel, sel_task)
        && let MenuRow::Item { detail, .. } = &mut view.rows[sel]
    {
        attach_tail(state, task, detail);
    }

    view.actions = vec![MenuAction::new(KILL_KEY, "kill").enabled(killable)];
    // `enter` opens the selected task's output — name the verb on the hint
    // edge rather than leaving it implicit (§5.9).
    view.enter_label = Some("output");
    view.empty = EmptyState::new(
        "no background tasks",
        "jobs the agent runs in the background land here",
    );
    view
}

/// `enter` — the task's output in the side panel (`PanelTarget::Task` +
/// `Action::LoadPanel`; the daemon's `GetTaskOutput` fills
/// `panel.task_lines`). The menu closes like a picker: `focus` moves to
/// `Panel` and the stack dies with the overlay.
pub fn on_row(
    state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let Some(task) = state.rail.task(&TaskId::new(row.as_str())) else {
        return vec![];
    };
    let target = PanelTarget::Task(task.id.clone());
    state.panel.target = Some(target.clone());
    state.focus = Focus::Panel {
        target,
        follow: false,
        follow_paused: false,
    };
    state.panel.scroll = 0;
    vec![Action::LoadPanel]
}

/// `k` — `KillTask` on the selected row (§5.9). The `RowId` *is* the daemon
/// id, so the request names the task under the cursor even while the list
/// reorders beneath it. Terminal rows get nothing — the daemon would only
/// no-op them (the action is also declared disabled on them, so this guard
/// mostly covers a stale view).
pub fn on_action(
    state: &mut AppState,
    _ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    if action.key != KILL_KEY {
        return vec![];
    }
    let Some(row) = row else {
        return vec![];
    };
    let Some(task) = state.rail.task(&TaskId::new(row.as_str())) else {
        return vec![];
    };
    if task.status.is_terminal() {
        return vec![];
    }
    vec![Action::Send(Request::KillTask(task.id.clone()))]
}

/// What the detail card needs for the current selection — recorded by frame
/// preparation into `rail.task_tail_want`; `None` while
/// the cursor rests on nothing or the task reports no output yet.
/// `output_bytes` doubles as the version: a `TaskUpdated` carrying more
/// output re-keys the want and re-arms the read by itself.
pub fn tail_want(state: &AppState, view: &MenuView, ui: &MenuUiState) -> Option<TaskTailKey> {
    let sel = resolve_cursor(&view.rows, &ui.cursor)?;
    let id = view.rows[sel].id()?;
    let task = state.rail.task(&TaskId::new(id.as_str()))?;
    (task.output_bytes > 0).then(|| TaskTailKey {
        task: task.id.clone(),
        output_bytes: task.output_bytes,
    })
}

/// The deferred read behind `rail.task_tail` (§5.9): the last ≤
/// [`TAIL_CAP_BYTES`] of `path` — always the file's end. `text` is `None`
/// when the spool file is missing or unreadable — a task whose output went
/// nowhere is not an error. Runs on a blocking thread via
/// `dispatch_deferred`, never inside `ui::draw` (§8.1).
pub(crate) fn read_tail(path: &Path) -> (Option<String>, bool) {
    let result = std::fs::File::open(path).and_then(|mut file| {
        let len = file.metadata()?.len();
        let start = len.saturating_sub(TAIL_CAP_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok((buf, start > 0))
    });
    match result {
        Ok((buf, truncated)) => {
            // `from_utf8_lossy` absorbs a multi-byte char cut at the
            // window's first byte; the window may also open mid-line —
            // the fragment goes. ANSI escapes go too: the card paints
            // text, not escape sequences.
            let text = crate::ui::toolcell::strip_ansi(&String::from_utf8_lossy(&buf));
            let text = if truncated {
                match text.find('\n') {
                    Some(i) => text[i + 1..].to_string(),
                    // A single over-long line still shows its tail.
                    None => text,
                }
            } else {
                text
            };
            (Some(text), truncated)
        }
        Err(_) => (None, false),
    }
}

/// One row per task: `RowId` is the daemon id — the cursor survives
/// re-sorts and arrivals by construction (§5.9). The label carries the
/// short command; the full `command` lives on the detail card.
fn task_row(task: &TaskInfo, now: u64, glyphs: &pacode_render::Glyphs) -> MenuRow {
    MenuRow::Item {
        id: RowId::new(task.id.as_str()),
        mark: status_mark(task.status),
        label: task.label.clone(),
        right: Some(right_tail(task, now, glyphs)),
        detail: task_detail(task, now),
    }
}

/// `◍` running, `✓` done, `✗` failed, `■` killed — killed reads as stopped
/// on purpose: the user ended it, it is not a failure.
fn status_mark(status: TaskStatus) -> Mark {
    match status {
        TaskStatus::Running => Mark::Running,
        TaskStatus::Completed => Mark::Ok,
        TaskStatus::Failed => Mark::Fail,
        TaskStatus::Killed => Mark::Stopped,
    }
}

/// The word the detail card's `status` field and a bar-less right tail use.
fn status_word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "running",
        TaskStatus::Completed => "done",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
    }
}

/// The right-hand cell (§5.9): while the task runs it is the progress
/// mini-bar or its short label plus the duration; once terminal it is the
/// outcome word plus the duration, with `N err` and a non-zero `exit N`
/// riding along.
fn right_tail(task: &TaskInfo, now: u64, glyphs: &pacode_render::Glyphs) -> String {
    let dur = format_duration_ms(task.duration_ms(now));
    let mut out = match task.status {
        TaskStatus::Running => {
            let head = progress_bar(task, glyphs)
                .or_else(|| task.progress.as_ref().and_then(|p| p.short_label()));
            match head {
                Some(h) => format!("{h} · {dur}"),
                None => format!("running · {dur}"),
            }
        }
        TaskStatus::Completed => format!("done · {dur}"),
        TaskStatus::Failed => format!("failed · {dur}"),
        TaskStatus::Killed => format!("killed · {dur}"),
    };
    if task.errors > 0 {
        out.push_str(&format!(" · {} err", task.errors));
    }
    if let Some(code) = task.exit_code
        && code != 0
    {
        out.push_str(&format!(" · exit {code}"));
    }
    out
}

/// `▰▰▰▱▱▱ 56%` — only while a percent is actually known: the reported
/// `percent`, or `current/total` normalised to one. A message-only progress
/// has no number to fill cells with — the bar stays off (§5.9: no bar when
/// progress is unknown).
fn progress_bar(task: &TaskInfo, glyphs: &pacode_render::Glyphs) -> Option<String> {
    let p = task.progress.as_ref()?;
    let percent = p.percent.or_else(|| match (p.current, p.total) {
        (Some(c), Some(t)) if t > 0 => Some((c as f64 / t as f64 * 100.0) as f32),
        _ => None,
    })?;
    let bar = glyphs.progress_bar(percent.clamp(0.0, 100.0).round() as u8, BAR_CELLS);
    let label = p.short_label()?;
    Some(format!("{bar} {label}"))
}

/// The card behind a row: the full command (the label is its two-word
/// cut), status and duration, progress and the log counters when they have
/// something to say — and, appended by [`attach_tail`] on the selected row
/// only, the spool file's tail.
fn task_detail(task: &TaskInfo, now: u64) -> Detail {
    let mut lines = vec![
        field("command", task.command.clone()),
        field("status", status_word(task.status).to_string()),
        field("duration", format_duration_ms(task.duration_ms(now))),
    ];
    if let Some(p) = task.progress.as_ref().and_then(|p| p.short_label()) {
        lines.push(field("progress", p));
    }
    if let Some(code) = task.exit_code {
        lines.push(field("exit code", format!("{code}")));
    }
    if task.warnings > 0 || task.errors > 0 {
        lines.push(field(
            "log",
            format!("{} warnings · {} errors", task.warnings, task.errors),
        ));
    }
    lines.push(field("cwd", crate::ui::overlays::short_path(&task.cwd)));
    if task.output_bytes > 0 {
        lines.push(field(
            "output",
            format!(
                "{} · {}",
                crate::ui::overlays::short_path(&task.output_path),
                fmt_bytes(task.output_bytes),
            ),
        ));
    }
    Detail {
        title: task.label.clone(),
        lines,
        // The chip mirrors the menu-level `k`: it greys out with it on a
        // terminal row.
        actions: vec![MenuAction::new(KILL_KEY, "kill").enabled(!task.status.is_terminal())],
        search: vec![task.command.clone()],
    }
}

/// The output tail on the selected card — the deferred read's cache when
/// it names this task (a stale `output_bytes` still shows while the re-read
/// is in flight rather than flashing the placeholder), an honest note
/// while the blocking read is out or the file has nothing to show.
fn attach_tail(state: &AppState, task: &TaskInfo, detail: &mut Detail) {
    if task.output_bytes == 0 {
        detail
            .lines
            .push(DetailLine::Text("(no output)".to_string()));
        return;
    }
    match state.rail.task_tail.as_ref() {
        Some(tail) if tail.task == task.id => match tail.text.as_deref() {
            Some(text) if !text.is_empty() => {
                if tail.truncated {
                    detail.lines.push(DetailLine::Text(format!(
                        "… output cut — last {} KB shown",
                        TAIL_CAP_BYTES / 1024
                    )));
                }
                let lines: Vec<&str> = text.lines().collect();
                let cut = lines.len().saturating_sub(TAIL_MAX_LINES);
                if cut > 0 {
                    detail
                        .lines
                        .push(DetailLine::Text(format!("… {cut} more lines above")));
                }
                for l in &lines[cut..] {
                    detail.lines.push(DetailLine::Text((*l).to_string()));
                }
            }
            Some(_) => detail
                .lines
                .push(DetailLine::Text("(no output)".to_string())),
            // The daemon counted bytes but the spool read came back empty —
            // the file went away or refused a read; say so, not a void.
            None => detail
                .lines
                .push(DetailLine::Text("(output unreadable)".to_string())),
        },
        _ => detail
            .lines
            .push(DetailLine::Text("loading output…".to_string())),
    }
}

fn field(label: &str, value: String) -> DetailLine {
    DetailLine::Field {
        label: label.to_string(),
        value,
    }
}

/// `810 B`, `12.4 KB` — the detail card's byte sizes; bytes stay bytes
/// under 1 KB.
fn fmt_bytes(n: u64) -> String {
    if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}
