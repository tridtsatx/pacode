//! §5.9: rows keyed by the daemon's task id (the cursor survives re-sorts
//! and removals), `k` kills the selected task, `enter` opens its output
//! panel, the `▰▱` bar shows only while a percent is known, and the detail
//! card's tail is the spool file's last ≤ 2 KB — via the deferred cache,
//! never a read inside `view`.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::state::{ProgressSource, TaskProgress, TaskStatus};
use pacode_types::{AgentId, Config, Request, SessionId, TaskId, TaskInfo};

use super::*;
use crate::state::rail::{TaskTail, TaskTailKey};
use crate::state::{Focus, Overlay, PanelTarget};

fn state() -> AppState {
    let mut s = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    s.cwd = PathBuf::from("/repo");
    s
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// A task row in `rail.tasks`: `label` is the two-word cut of `command`.
fn task(id: &str, status: TaskStatus, started_at_ms: u64) -> TaskInfo {
    TaskInfo {
        id: TaskId::new(id),
        session: SessionId::new("ses_1"),
        owner: AgentId::main(),
        label: format!("{id} run"),
        command: format!("run {id} --all"),
        cwd: PathBuf::from("/repo"),
        status,
        backgrounded: true,
        exit_code: None,
        started_at_ms,
        ended_at_ms: if status.is_terminal() {
            Some(started_at_ms + 2_000)
        } else {
            None
        },
        progress: None,
        warnings: 0,
        errors: 0,
        output_path: PathBuf::from(format!("/tmp/{id}.log")),
        output_bytes: 0,
        acked: false,
    }
}

/// The `right` cell of the row whose `RowId` is `id`.
fn right_of(view: &MenuView, id: &str) -> Option<String> {
    view.rows.iter().find_map(|r| match r {
        MenuRow::Item { id: rid, right, .. } if rid.as_str() == id => right.clone(),
        _ => None,
    })
}

#[test]
fn rows_are_keyed_by_task_id() {
    let mut s = state();
    s.rail.upsert_task(task("t_a", TaskStatus::Running, 100));
    s.rail.upsert_task(task("t_b", TaskStatus::Completed, 50));

    let v = view(&s, &MenuUiState::default());
    assert_eq!(v.id, MenuId::Background);
    // `upsert_task` keeps `started_at_ms` order — t_b before t_a — and each
    // `RowId` is the daemon id, not an index.
    let ids: Vec<String> = v
        .rows
        .iter()
        .filter_map(|r| r.id().map(|i| i.as_str().to_string()))
        .collect();
    assert_eq!(ids, vec!["t_b", "t_a"]);
}

#[test]
fn foreground_tasks_do_not_appear_in_the_background_menu() {
    let mut s = state();
    s.rail
        .upsert_task(task("background", TaskStatus::Running, 100));
    let mut foreground = task("foreground", TaskStatus::Running, 200);
    foreground.backgrounded = false;
    s.rail.upsert_task(foreground);

    let v = view(&s, &MenuUiState::default());
    let ids: Vec<String> = v
        .rows
        .iter()
        .filter_map(|row| row.id().map(|id| id.as_str().to_string()))
        .collect();
    assert_eq!(ids, vec!["background"]);
}

#[test]
fn cursor_tracks_the_task_id_across_reorders_and_removals() {
    let mut s = state();
    s.rail.upsert_task(task("t_a", TaskStatus::Running, 100));
    s.rail.upsert_task(task("t_b", TaskStatus::Running, 200));
    let ui = MenuUiState {
        cursor: RowId::new("t_b"),
        ..MenuUiState::default()
    };

    // A task that sorts before both arrives: `t_b` is still the selection.
    s.rail.upsert_task(task("t_new", TaskStatus::Running, 50));
    let v = view(&s, &ui);
    let sel = resolve_cursor(&v.rows, &ui.cursor).unwrap();
    assert_eq!(v.rows[sel].id().unwrap().as_str(), "t_b");

    // `t_b` disappears: the cursor lands on the first selectable row (§5.9).
    s.rail.tasks.retain(|t| t.id.as_str() != "t_b");
    let v = view(&s, &ui);
    let sel = resolve_cursor(&v.rows, &ui.cursor).unwrap();
    assert_eq!(v.rows[sel].id().unwrap().as_str(), "t_new");
}

#[test]
fn k_kills_the_selected_task_by_id() {
    let mut s = state();
    s.rail.upsert_task(task("t_a", TaskStatus::Running, 100));
    s.rail.upsert_task(task("t_b", TaskStatus::Running, 200));

    crate::keys_menu::open(&mut s, MenuId::Background);
    assert!(matches!(s.focus, Focus::Overlay(Overlay::Menu { .. })));
    let taken = std::mem::replace(&mut s.focus, Focus::Normal);
    let Focus::Overlay(Overlay::Menu { mut stack, mut ui }) = taken else {
        panic!("menu did not open");
    };
    // Select t_b — by id, not index — then `k`.
    ui.cursor = RowId::new("t_b");
    let acts =
        crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('k')));
    assert_eq!(
        acts,
        vec![Action::Send(Request::KillTask(TaskId::new("t_b")))]
    );
    // `k` was the action, not cursor-up: the selection did not move.
    assert_eq!(ui.cursor, RowId::new("t_b"));
}

#[test]
fn k_on_a_terminal_task_is_not_a_kill() {
    let mut s = state();
    s.rail
        .upsert_task(task("t_done", TaskStatus::Completed, 100));
    s.rail.upsert_task(task("t_live", TaskStatus::Running, 200));

    crate::keys_menu::open(&mut s, MenuId::Background);
    let taken = std::mem::replace(&mut s.focus, Focus::Normal);
    let Focus::Overlay(Overlay::Menu { mut stack, mut ui }) = taken else {
        panic!("menu did not open");
    };
    // The completed task sits on top; `k` is disabled there and the
    // movement alias clamps at the edge — no request leaves.
    assert_eq!(ui.cursor, RowId::default());
    let acts =
        crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('k')));
    assert!(acts.is_empty());
    // …while the action stays declared, just disabled on that row.
    let v = view(&s, &ui);
    assert!(v.actions.iter().all(|a| !a.enabled));
    // `j` still moves to the live task — and `k` on it does fire.
    let _ = crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('j')));
    assert_eq!(ui.cursor, RowId::new("t_live"));
    let acts =
        crate::keys_menu::handle_menu_key(&mut s, &mut stack, &mut ui, key(KeyCode::Char('k')));
    assert_eq!(
        acts,
        vec![Action::Send(Request::KillTask(TaskId::new("t_live")))]
    );
}

#[test]
fn enter_opens_the_task_output_panel() {
    let mut s = state();
    s.rail.upsert_task(task("t_a", TaskStatus::Running, 100));

    crate::keys_menu::open(&mut s, MenuId::Background);
    let acts = crate::keys::handle_key(&mut s, key(KeyCode::Enter), Instant::now());

    assert_eq!(acts, vec![Action::LoadPanel]);
    assert_eq!(s.panel.target, Some(PanelTarget::Task(TaskId::new("t_a"))));
    assert!(matches!(
        s.focus,
        Focus::Panel {
            target: PanelTarget::Task(ref id),
            follow: false,
            follow_paused: false,
        } if id.as_str() == "t_a"
    ));
}

#[test]
fn progress_bar_only_when_the_percent_is_known() {
    let mut s = state();
    s.rail.upsert_task(task("t_none", TaskStatus::Running, 100));
    let mut msg = task("t_msg", TaskStatus::Running, 200);
    msg.progress = Some(TaskProgress {
        current: None,
        total: None,
        percent: None,
        message: Some("compiling".into()),
        source: ProgressSource::Parsed,
        updated_at_ms: 0,
    });
    s.rail.upsert_task(msg);
    let mut pct = task("t_pct", TaskStatus::Running, 300);
    pct.progress = Some(TaskProgress {
        current: Some(2),
        total: Some(10),
        percent: None,
        message: None,
        source: ProgressSource::Reported,
        updated_at_ms: 0,
    });
    s.rail.upsert_task(pct);

    let v = view(&s, &MenuUiState::default());
    // Unknown progress: a bare `running · <dur>` — no fabricated bar.
    let none = right_of(&v, "t_none").unwrap();
    assert!(none.starts_with("running · "));
    assert!(!none.contains('▰') && !none.contains('▱'));
    // Message-only progress shows the message — still no bar.
    let msg = right_of(&v, "t_msg").unwrap();
    assert!(msg.contains("compiling"));
    assert!(!msg.contains('▰'));
    // `current/total` yields a percent: bar + `short_label` counts.
    let pct = right_of(&v, "t_pct").unwrap();
    assert!(pct.contains('▰') && pct.contains('▱'));
    assert!(pct.contains("2/10"));
}

#[test]
fn right_tail_shows_the_outcome_after_the_task_ends() {
    let mut s = state();
    s.rail.upsert_task(task("t_ok", TaskStatus::Completed, 100));
    let mut bad = task("t_bad", TaskStatus::Failed, 200);
    bad.errors = 2;
    bad.exit_code = Some(1);
    s.rail.upsert_task(bad);
    s.rail.upsert_task(task("t_k", TaskStatus::Killed, 300));

    let v = view(&s, &MenuUiState::default());
    assert!(right_of(&v, "t_ok").unwrap().starts_with("done · "));
    assert_eq!(
        right_of(&v, "t_bad").unwrap(),
        "failed · 2.0s · 2 err · exit 1"
    );
    assert_eq!(right_of(&v, "t_k").unwrap(), "killed · 2.0s");
}

#[test]
fn read_tail_keeps_the_last_two_kib_and_says_it_cut() {
    let dir = std::env::temp_dir().join(format!("pacode_tail_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("out.log");
    // ~6.5 KB of numbered lines — the tail must be the file's end.
    let mut body = String::new();
    for i in 0..200 {
        body.push_str(&format!("line {i:04} padding padding padding\n"));
    }
    std::fs::write(&file, &body).unwrap();

    let (text, truncated) = read_tail(&file);
    let text = text.unwrap();
    assert!(truncated);
    assert!(text.len() <= TAIL_CAP_BYTES as usize);
    assert!(text.ends_with("line 0199 padding padding padding\n"));
    assert!(!text.contains("line 0000"));
    // The window opened mid-line — the first kept line is whole.
    assert!(text.lines().next().unwrap().starts_with("line "));

    // A small file is kept whole and not marked cut.
    let small = dir.join("small.log");
    std::fs::write(&small, "a\nb\n").unwrap();
    let (text, truncated) = read_tail(&small);
    assert_eq!(text.as_deref(), Some("a\nb\n"));
    assert!(!truncated);

    // ANSI escapes never reach the card.
    let ansi = dir.join("ansi.log");
    std::fs::write(&ansi, "\u{1b}[31mred\u{1b}[0m\nplain\n").unwrap();
    let (text, _) = read_tail(&ansi);
    let text = text.unwrap();
    assert!(text.contains("red") && text.contains("plain"));
    assert!(!text.contains('\u{1b}'));

    // A missing file is `None`, not a panic.
    let (text, truncated) = read_tail(&dir.join("ghost.log"));
    assert!(text.is_none());
    assert!(!truncated);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn detail_card_shows_the_cached_tail() {
    let mut s = state();
    let mut t = task("t_a", TaskStatus::Completed, 100);
    t.output_bytes = 4096;
    s.rail.upsert_task(t);
    let ui = MenuUiState::default();

    // Nothing cached yet — an honest placeholder while the read is out.
    let v = view(&s, &ui);
    let detail = v.rows[0].detail().unwrap();
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t == "loading output…"))
    );

    // The deferred job landed: cut marker + the tail's lines.
    s.rail.task_tail = Some(TaskTail {
        task: TaskId::new("t_a"),
        output_bytes: 4096,
        text: Some("tail line one\ntail line two".to_string()),
        truncated: true,
    });
    let v = view(&s, &ui);
    let detail = v.rows[0].detail().unwrap();
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("output cut")))
    );
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t == "tail line two"))
    );
}

#[test]
fn detail_card_reports_no_output_for_an_empty_spool() {
    let mut s = state();
    s.rail.upsert_task(task("t_a", TaskStatus::Running, 100)); // output_bytes 0
    let v = view(&s, &MenuUiState::default());
    let detail = v.rows[0].detail().unwrap();
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t == "(no output)"))
    );
}

#[test]
fn tail_want_keys_on_task_and_output_bytes() {
    let mut s = state();
    let mut t = task("t_a", TaskStatus::Running, 100);
    t.output_bytes = 42;
    s.rail.upsert_task(t);

    let v = view(&s, &MenuUiState::default());
    assert_eq!(
        tail_want(&s, &v, &MenuUiState::default()),
        Some(TaskTailKey {
            task: TaskId::new("t_a"),
            output_bytes: 42,
        })
    );

    // A task reporting no output arms no read at all.
    s.rail.tasks[0].output_bytes = 0;
    let v = view(&s, &MenuUiState::default());
    assert_eq!(tail_want(&s, &v, &MenuUiState::default()), None);
}

#[test]
fn on_action_ignores_foreign_keys_and_unknown_rows() {
    let mut s = state();
    s.rail.upsert_task(task("t_a", TaskStatus::Running, 100));
    let other = MenuAction::new(
        KeyBinding {
            code: KeyCode::Char('x'),
            mods: KeyModifiers::NONE,
        },
        "noop",
    );
    let mut ui = MenuUiState::default();
    assert!(on_action(&mut s, &mut ui, &other, Some(&RowId::new("t_a"))).is_empty());
    // The right key on a row the state no longer has still sends nothing.
    let kill = MenuAction::new(KILL_KEY, "kill");
    assert!(on_action(&mut s, &mut ui, &kill, Some(&RowId::new("ghost"))).is_empty());
    assert!(on_action(&mut s, &mut ui, &kill, None).is_empty());
}

#[test]
fn empty_state_when_no_tasks() {
    let s = state();
    let v = view(&s, &MenuUiState::default());
    assert!(v.rows.is_empty());
    assert_eq!(v.empty.primary, "no background tasks");
    // `k` disables with the list.
    assert!(v.actions.iter().all(|a| !a.enabled));
}
