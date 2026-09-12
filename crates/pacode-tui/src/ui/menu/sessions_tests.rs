//! §5.6: recency groups on a fixed `now`, the char-safe preview cap, the
//! local filter hitting title and id, and `enter` → `Attach::Resume`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::time::now_ms;
use pacode_types::{Config, Effort, Mode, ModelRoute, SessionId, SessionMeta};

use super::*;
use crate::keys_menu::{handle_menu_key, open};
use crate::state::{Focus, Overlay};

fn session(id: &str, updated_ms: u64) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        name: None,
        cwd: std::path::PathBuf::from("/work/proj"),
        git_branch: None,
        created_at_ms: updated_ms.saturating_sub(3_600_000),
        updated_at_ms: updated_ms,
        model: ModelRoute::new("prov", "model-x"),
        effort: Effort::High,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

fn make_state(sessions: Vec<SessionMeta>) -> AppState {
    let mut state = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    state.sessions = sessions;
    state
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Section labels in row order — the list shape without the items.
fn section_labels(view: &MenuView) -> Vec<String> {
    view.rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Section { label, .. } => Some(label.clone()),
            MenuRow::Item { .. }
            | MenuRow::Toggle { .. }
            | MenuRow::Value { .. }
            | MenuRow::Submenu { .. }
            | MenuRow::Check { .. }
            | MenuRow::Rich { .. } => None,
        })
        .collect()
}

#[test]
fn group_boundaries_hold_on_a_fixed_day_start() {
    let day_start = 1_000_000_000_000u64;
    // The boundary itself is today; a millisecond before it is the week.
    assert_eq!(group_of(day_start, day_start), Group::Today);
    assert_eq!(
        group_of(day_start + 12 * 3_600_000, day_start),
        Group::Today
    );
    // Clock skew ahead of midnight still reads as today, never a panic.
    assert_eq!(group_of(day_start + 10 * DAY_MS, day_start), Group::Today);
    assert_eq!(group_of(day_start - 1, day_start), Group::Week);
    // The week edge is inclusive; one ms older falls to earlier.
    assert_eq!(group_of(day_start - WEEK_MS, day_start), Group::Week);
    assert_eq!(group_of(day_start - WEEK_MS - 1, day_start), Group::Earlier);
    assert_eq!(group_of(0, day_start), Group::Earlier);
}

#[test]
fn rows_group_into_three_sections_in_order() {
    let now = now_ms();
    // Timezone-proof picks: `day_start` sits in (now − 24h, now], so `now`
    // is always Today; `now − 3d` is always < day_start yet ≥ day_start −
    // 7d (Week); `now − 8d` is always < day_start − 7d (Earlier).
    let state = make_state(vec![
        session("pacode-old", now.saturating_sub(8 * DAY_MS)),
        session("pacode-new", now),
        session("pacode-mid", now.saturating_sub(3 * DAY_MS)),
    ]);
    let view = view(&state, &MenuUiState::default());
    assert_eq!(section_labels(&view), vec!["today", "this week", "earlier"]);
    // Inside each section the daemon's `updated_at DESC` order holds even
    // though the input vector was scrambled.
    let ids: Vec<&str> = view
        .rows
        .iter()
        .filter_map(|r| r.id().map(|i| i.as_str()))
        .collect();
    assert_eq!(ids, vec!["pacode-new", "pacode-mid", "pacode-old"]);
}

#[test]
fn day_start_is_the_local_midnight() {
    // 2026-09-12 15:04 local — the start of that day is local midnight.
    let noon = Local
        .with_ymd_and_hms(2026, 9, 12, 15, 4, 0)
        .single()
        .expect("valid local time");
    let now = u64::try_from(noon.timestamp_millis()).expect("post-epoch");
    let want = Local
        .with_ymd_and_hms(2026, 9, 12, 0, 0, 0)
        .single()
        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
        .expect("midnight resolves");
    assert_eq!(day_start(now), want);
    assert_eq!(day_start(0), 0);
}

#[test]
fn row_carries_title_branch_and_age() {
    let now = now_ms();
    let mut s = session("pacode-1", now.saturating_sub(2 * 3_600_000));
    s.git_branch = Some("feat/x".to_string());
    s.first_prompt = Some("fix the flaky test".to_string());
    let state = make_state(vec![s]);
    let view = view(&state, &MenuUiState::default());
    let [
        MenuRow::Section { .. },
        MenuRow::Item {
            label,
            right,
            detail,
            ..
        },
    ] = view.rows.as_slice()
    else {
        panic!("section + one item expected, got {:?}", view.rows);
    };
    assert_eq!(label, "fix the flaky test");
    assert_eq!(right.as_deref(), Some("feat/x · 2h ago"));
    // The id is searchable without being on the row (§5.6).
    assert!(detail.search.iter().any(|t| t == "pacode-1"));
    assert!(detail.lines.iter().any(
        |l| matches!(l, DetailLine::Field { label, value } if label == "id" && value == "pacode-1")
    ));
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Field { label, value } if label == "branch" && value == "feat/x"))
    );
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Field { label, value } if label == "model" && value.contains("prov/model-x")))
    );
}

#[test]
fn preview_caps_at_three_lines_on_char_boundaries() {
    // Many lines: the cap keeps three and marks the cut.
    let many = cap_lines("one\ntwo\nthree\nfour\nfive", PREVIEW_MAX_LINES);
    assert_eq!(many, vec!["one", "two", "three…"]);

    // A long line of multibyte chars is cut by chars — no mid-UTF-8 split,
    // no panic — and gets the ellipsis.
    let wide = "界".repeat(PREVIEW_LINE_CHARS + 40);
    let cut = cap_lines(&wide, PREVIEW_MAX_LINES);
    assert_eq!(cut.len(), 1);
    let body = cut[0].strip_suffix('…').expect("cut lines end with …");
    assert_eq!(body.chars().count(), PREVIEW_LINE_CHARS);

    // A line exactly at the cap is not marked.
    let exact = "é".repeat(PREVIEW_LINE_CHARS);
    let kept = cap_lines(&exact, PREVIEW_MAX_LINES);
    assert_eq!(kept, vec![exact]);

    // Two lines fit unmarked; empty text yields nothing.
    assert_eq!(cap_lines("a\nb", PREVIEW_MAX_LINES), vec!["a", "b"]);
    assert!(cap_lines("", PREVIEW_MAX_LINES).is_empty());
}

#[test]
fn preview_lands_in_the_detail_labeled_as_first_prompt() {
    let now = now_ms();
    let mut s = session("pacode-9", now);
    s.first_prompt = Some("line one\nline two\nline three\nline four".to_string());
    let state = make_state(vec![s]);
    let view = view(&state, &MenuUiState::default());
    let Some(detail) = view.rows[1].detail() else {
        panic!("item detail expected");
    };
    // "first prompt: line one" then two more text lines — the fourth is cut.
    assert!(
        detail
            .lines
            .iter()
            .any(|l| matches!(l, DetailLine::Field { label, value } if label == "first prompt" && value == "line one"))
    );
    let texts: Vec<&str> = detail
        .lines
        .iter()
        .filter_map(|l| match l {
            DetailLine::Text(t) => Some(t.as_str()),
            DetailLine::Field { .. } | DetailLine::Code { .. } => None,
        })
        .collect();
    assert_eq!(texts, vec!["line two", "line three…"]);
}

#[test]
fn local_filter_matches_title_and_id() {
    let now = now_ms();
    let mut a = session("pacode-abc", now);
    a.first_prompt = Some("fix login bug".to_string());
    let mut b = session("pacode-xyz", now);
    b.name = Some("docs rework".to_string());
    let state = make_state(vec![a, b]);

    // Title hit — `apply_filter` is what `view_for` runs for Local.
    let mut v = view(&state, &MenuUiState::default());
    v.apply_filter("login");
    let kept: Vec<&str> = v
        .rows
        .iter()
        .filter_map(|r| r.id().map(|i| i.as_str()))
        .collect();
    assert_eq!(kept, vec!["pacode-abc"]);

    // Id hit through the detail's search terms.
    let mut v = view(&state, &MenuUiState::default());
    v.apply_filter("xyz");
    let kept: Vec<&str> = v
        .rows
        .iter()
        .filter_map(|r| r.id().map(|i| i.as_str()))
        .collect();
    assert_eq!(kept, vec!["pacode-xyz"]);

    // A miss leaves no rows — and no orphaned section headers either.
    let mut v = view(&state, &MenuUiState::default());
    v.apply_filter("nope");
    assert!(v.rows.is_empty());
}

#[test]
fn enter_resumes_and_closes_the_stack() {
    let mut state = make_state(vec![session("pacode-42", now_ms())]);
    let mut stack = MenuStack::root(MenuId::Sessions);
    let mut ui = MenuUiState::default();
    let actions = handle_menu_key(&mut state, &mut stack, &mut ui, key(KeyCode::Enter));
    assert_eq!(
        actions,
        vec![Action::Send(Request::Attach(Attach::Resume {
            session: SessionId::new("pacode-42"),
        }))]
    );
    assert!(stack.is_empty());
}

#[test]
fn enter_on_a_gone_row_is_a_noop() {
    let mut state = make_state(vec![session("pacode-1", now_ms())]);
    let mut stack = MenuStack::root(MenuId::Sessions);
    let mut ui = MenuUiState::default();
    let actions = on_row(&mut state, &mut stack, &mut ui, &RowId::new("pacode-ghost"));
    assert!(actions.is_empty());
    assert!(!stack.is_empty());
}

#[test]
fn open_pushes_one_level_and_asks_for_the_list() {
    let mut state = make_state(vec![session("pacode-1", now_ms())]);
    let actions = open(&mut state, MenuId::Sessions);
    let Focus::Overlay(Overlay::Menu { stack, .. }) = &state.focus else {
        panic!("expected the menu overlay, got {:?}", state.focus);
    };
    assert_eq!(stack.depth(), 1);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::Sessions));
    assert_eq!(
        actions,
        vec![Action::Send(Request::ListSessions { limit: LIST_LIMIT })]
    );
}

#[test]
fn empty_and_badge_states() {
    let mut state = make_state(vec![]);
    let v = view(&state, &MenuUiState::default());
    assert!(v.rows.is_empty());
    assert_eq!(v.empty.primary, "no sessions yet");
    assert!(v.status.is_none());

    // In-flight refresh on an empty list — the honest "loading…".
    state.lists.mark_sent(crate::cache::ListKind::Sessions);
    let v = view(&state, &MenuUiState::default());
    assert_eq!(v.status, Some(StatusLine::loading()));
    assert_eq!(v.empty.primary, "loading…");

    // A live query with no hits — the miss text, not "no sessions yet".
    state.lists.on_reply(crate::cache::ListKind::Sessions, true);
    let ui = MenuUiState {
        query: "zzz".to_string(),
        ..MenuUiState::default()
    };
    let v = view(&state, &ui);
    assert_eq!(v.empty.primary, "no sessions match");
}
