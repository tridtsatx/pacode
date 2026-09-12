//! §5.10 coverage: the lane graph through `pacode_vcs::lanes` (merge and
//! two-branch histories, the six-lane `»` cap, the ASCII fallback), ref
//! badges in their per-kind tones, the want-driven `--stat` fetch and page
//! append, `enter` → commit-diff panel, `ctrl+b` → the refs level, and the
//! EmptyState conditions.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use pacode_types::Config;
use pacode_types::vcs::{CommitRow, GitStatus, RefBadge};

use super::*;
use crate::binding::Binding;
use crate::state::Focus;
use crate::ui::menu::{LabelTone, MenuStack, RowId};

fn state() -> AppState {
    let mut s = AppState::new(Config::default(), "0.1.0".into(), 120, 34);
    s.cwd = PathBuf::from("/repo");
    s
}

/// A commit row with a 40-hex-shaped hash so `commit:` ids and `lanes()`
/// treat it like the real thing.
fn commit(hash: &str, parents: &[&str], subject: &str, refs: Vec<RefBadge>) -> CommitRow {
    CommitRow {
        hash: hash.to_string(),
        short: hash.chars().take(7).collect(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        refs,
        subject: subject.to_string(),
        body: String::new(),
        author: "Ada".to_string(),
        date_ms: now_ms().saturating_sub(3_600_000),
    }
}

fn h(ch: char) -> String {
    ch.to_string().repeat(40)
}

/// The graph column of row `i` — the first `Faint` span, trimmed.
fn graph_of(view: &MenuView, i: usize) -> String {
    match &view.rows[i] {
        MenuRow::Rich { spans, .. } => spans
            .first()
            .map(|s| s.text.trim_end().to_string())
            .unwrap_or_default(),
        other => panic!("row {i} is not Rich: {other:?}"),
    }
}

/// `(text, tone)` of every badge span — the pieces between the graph and the
/// subject, separators excluded.
fn badges_of(view: &MenuView, i: usize) -> Vec<(String, LabelTone)> {
    match &view.rows[i] {
        MenuRow::Rich { spans, .. } => spans
            .iter()
            .skip(1)
            .take_while(|s| s.tone != LabelTone::Text)
            .filter(|s| s.text != " ")
            .map(|s| (s.text.clone(), s.tone))
            .collect(),
        other => panic!("row {i} is not Rich: {other:?}"),
    }
}

/// Draw the level into a test backend at `w` columns and return the rows.
fn screen(state: &AppState, ui: &MenuUiState, w: u16, h: u16) -> Vec<String> {
    let view = view(state, ui);
    let opts = pacode_render::RenderOptions::new(w, state.config.ui.ascii_only);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| crate::ui::menu::draw(f, f.area(), &view, ui, &opts))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            row.trim_end().to_string()
        })
        .collect()
}

#[test]
fn a_merge_draws_node_fork_and_closing_lane() {
    // M merges A and B; both share parent C — the classic diamond.
    let mut s = state();
    s.git.commits = vec![
        commit(&h('m'), &[&h('a'), &h('b')], "merge", vec![]),
        commit(&h('a'), &[&h('c')], "on main", vec![]),
        commit(&h('b'), &[&h('c')], "on side", vec![]),
        commit(&h('c'), &[], "root", vec![]),
    ];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    assert_eq!(graph_of(&v, 0), "●╰", "merge opens the side lane");
    assert_eq!(graph_of(&v, 1), "●│", "main lane, side lane waits");
    assert_eq!(graph_of(&v, 2), "│●", "the side commit is on lane 1");
    assert_eq!(graph_of(&v, 3), "●╯", "the side lane closes into the root");
}

#[test]
fn two_branches_keep_parallel_lanes() {
    // main: N → O → P; feature branched at P: F → G, merged nowhere visible.
    let mut s = state();
    s.git.commits = vec![
        commit(&h('n'), &[&h('o')], "main 2", vec![]),
        commit(&h('g'), &[&h('f')], "side 2", vec![]),
        commit(&h('o'), &[&h('p')], "main 1", vec![]),
        commit(&h('f'), &[&h('p')], "side 1", vec![]),
        commit(&h('p'), &[], "base", vec![]),
    ];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    // The side lane cannot exist above its tip: it opens at `g`'s row.
    assert_eq!(graph_of(&v, 0), "●", "only main's lane so far");
    assert_eq!(graph_of(&v, 1), "│●", "side commit on its own lane");
    assert_eq!(graph_of(&v, 2), "●│");
    assert_eq!(graph_of(&v, 3), "│●");
    // Both lanes close on the shared base: the side lane merges into it.
    assert_eq!(graph_of(&v, 4), "●╯");
}

#[test]
fn the_six_lane_cap_overflows_as_raquo() {
    // Seven commits all sharing a parent below the window — lane 7 folds.
    let mut s = state();
    s.git.commits = (0..7u32)
        .map(|i| {
            commit(
                &format!("{:040x}", i + 1),
                &[&"9".repeat(40)],
                &format!("tip {i}"),
                vec![],
            )
        })
        .collect();
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    // Each new head forks another lane; the seventh cannot fit six columns.
    assert!(
        v.rows.iter().any(|r| match r {
            MenuRow::Rich { spans, .. } => {
                spans.first().is_some_and(|s| s.text.contains('»'))
            }
            _ => false,
        }),
        "a history wider than six lanes folds into »"
    );
    // And no row ever prints more than six lane columns plus the fold.
    for (i, _) in s.git.commits.iter().enumerate() {
        let g = graph_of(&v, i);
        assert!(g.chars().count() <= 7, "row {i} graph overflows: {g:?}");
    }
}

#[test]
fn ascii_only_uses_the_fallback_glyphs() {
    let mut s = state();
    s.config.ui.ascii_only = true;
    s.git.commits = vec![
        commit(&h('m'), &[&h('a'), &h('b')], "merge", vec![]),
        commit(&h('a'), &[&h('c')], "on main", vec![]),
        commit(&h('b'), &[&h('c')], "on side", vec![]),
        commit(&h('c'), &[], "root", vec![]),
    ];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    assert_eq!(graph_of(&v, 0), "*\\", "ascii fork");
    assert_eq!(graph_of(&v, 1), "*|");
    assert_eq!(graph_of(&v, 2), "|*");
    assert_eq!(graph_of(&v, 3), "*/", "ascii merge close");
}

#[test]
fn ref_badges_carry_their_own_tones() {
    let mut s = state();
    s.git.commits = vec![commit(
        &h('a'),
        &[],
        "release",
        vec![
            RefBadge::Head,
            RefBadge::Branch("main".into()),
            RefBadge::Remote("origin/main".into()),
            RefBadge::Tag("v1.0".into()),
        ],
    )];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    assert_eq!(
        badges_of(&v, 0),
        vec![
            ("HEAD→main".to_string(), LabelTone::Accent),
            ("origin/main".to_string(), LabelTone::Cyan),
            ("v1.0".to_string(), LabelTone::Yellow),
        ],
        "HEAD merges with its branch; remote and tag keep their own tones"
    );
}

#[test]
fn a_lone_branch_badge_is_green_and_detached_head_is_accent() {
    let mut s = state();
    s.git.commits = vec![
        commit(&h('a'), &[], "tip", vec![RefBadge::Branch("dev".into())]),
        commit(&h('b'), &[], "detached", vec![RefBadge::Head]),
    ];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    assert_eq!(
        badges_of(&v, 0),
        vec![("dev".to_string(), LabelTone::Green)]
    );
    assert_eq!(
        badges_of(&v, 1),
        vec![("HEAD".to_string(), LabelTone::Accent)]
    );
    // ASCII mode prints `->` in the merged badge.
    s.config.ui.ascii_only = true;
    s.git.commits[0].refs = vec![RefBadge::Head, RefBadge::Branch("dev".into())];
    let v = view(&s, &MenuUiState::default());
    assert_eq!(
        badges_of(&v, 0),
        vec![("HEAD->dev".to_string(), LabelTone::Accent)]
    );
}

#[test]
fn the_row_lays_out_at_60_100_and_160() {
    for w in [60u16, 100, 160] {
        let mut s = state();
        s.git.commits = vec![
            commit(
                &h('a'),
                &[&h('b')],
                "feat: the subject",
                vec![RefBadge::Branch("main".into())],
            ),
            commit(&h('b'), &[], "fix: parent", vec![]),
        ];
        s.git.status = Some(GitStatus::Ready);
        let text = screen(&s, &MenuUiState::default(), w, 24).join("\n");
        assert!(text.contains("●"), "graph at {w}: {text}");
        assert!(text.contains("feat:"), "subject at {w}: {text}");
        assert!(text.contains("aaaaaaa"), "short hash at {w}: {text}");
        assert!(text.contains("main"), "branch badge at {w}: {text}");
    }
}

#[test]
fn enter_opens_the_commit_diff_panel() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Git);
    let mut ui = MenuUiState::default();
    let actions = on_row(
        &mut s,
        &mut stack,
        &mut ui,
        &RowId::new(format!("commit:{}", h('a'))),
    );
    assert_eq!(actions, vec![Action::GitDiff { hash: h('a') }]);
    assert_eq!(
        s.focus,
        Focus::Panel {
            target: PanelTarget::CommitDiff { hash: h('a') },
            follow: false,
            follow_paused: false,
        }
    );
    assert_eq!(s.panel.scroll, usize::MAX, "a diff reads from its head");
    // A non-commit row id is a no-op.
    assert!(on_row(&mut s, &mut stack, &mut ui, &RowId::new("x")).is_empty());
}

#[test]
fn ctrl_b_pushes_the_branches_level() {
    let mut s = state();
    let mut stack = MenuStack::root(MenuId::Git);
    let mut ui = MenuUiState::default();
    let action = MenuAction::new(BRANCHES_KEY, "branches");
    let actions = on_action(&mut s, &mut stack, &mut ui, &action, None);
    assert!(actions.is_empty(), "a descend is pure stack state");
    assert_eq!(stack.depth(), 2);
    assert_eq!(stack.current().map(|l| l.id), Some(MenuId::GitBranches));
    // A foreign action is ignored.
    let mut stack = MenuStack::root(MenuId::Git);
    let other = MenuAction::new(
        Binding {
            code: KeyCode::Char('x'),
            mods: KeyModifiers::NONE,
        },
        "nope",
    );
    on_action(&mut s, &mut stack, &mut ui, &other, None);
    assert_eq!(stack.depth(), 1);
}

#[test]
fn the_last_row_wants_one_more_page_and_only_while_has_more() {
    let mut s = state();
    s.git.commits = vec![
        commit(&h('a'), &[&h('b')], "newer", vec![]),
        commit(&h('b'), &[], "older", vec![]),
    ];
    s.git.status = Some(GitStatus::Ready);
    s.git.has_more = true;
    let v = view(&s, &MenuUiState::default());
    let ui = MenuUiState {
        cursor: RowId::new(format!("commit:{}", h('b'))),
        ..MenuUiState::default()
    };
    let (_, more) = wants(&s, &v, &ui, false);
    assert_eq!(more, Some(h('b')), "after = the cursor's hash");
    // Mid-list: no page want.
    let ui_mid = MenuUiState {
        cursor: RowId::new(format!("commit:{}", h('a'))),
        ..MenuUiState::default()
    };
    assert_eq!(wants(&s, &v, &ui_mid, false).1, None);
    // History exhausted: never ask again (§5.10).
    s.git.has_more = false;
    assert_eq!(wants(&s, &v, &ui, false).1, None);
}

#[test]
fn the_stat_want_follows_the_detail_pane() {
    let mut s = state();
    s.git.commits = vec![commit(&h('a'), &[], "one", vec![])];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    let ui = MenuUiState::default();
    // Wide: the selected commit's stat is wanted; narrow: nothing to show
    // it in, so no fetch is armed.
    assert_eq!(wants(&s, &v, &ui, true).0, Some(h('a')));
    assert_eq!(wants(&s, &v, &ui, false).0, None);
}

#[test]
fn the_selected_card_shows_the_landed_stat() {
    let mut s = state();
    s.git.commits = vec![commit(&h('a'), &[], "one", vec![])];
    s.git.status = Some(GitStatus::Ready);
    let v = view(&s, &MenuUiState::default());
    // Nothing fetched yet: the honest placeholder.
    let d = v.rows[0].detail().expect("detail");
    assert!(
        d.lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t == "loading stat…")),
        "loading line: {d:?}"
    );
    // The reply landed for this hash: the stat lines join the card.
    s.git
        .apply_show(h('a'), " src/a.rs | 2 +-\n 1 file changed".to_string());
    let v = view(&s, &MenuUiState::default());
    let d = v.rows[0].detail().expect("detail");
    assert!(
        d.lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("src/a.rs"))),
        "stat line: {d:?}"
    );
    // A failed fetch names its error instead of loading forever.
    s.git.fail_show(h('a'), "timeout".to_string());
    let v = view(&s, &MenuUiState::default());
    let d = v.rows[0].detail().expect("detail");
    assert!(
        d.lines
            .iter()
            .any(|l| matches!(l, DetailLine::Text(t) if t.contains("timeout"))),
        "error line: {d:?}"
    );
}

#[test]
fn not_a_repo_and_missing_git_are_empty_states() {
    let mut s = state();
    s.git.status = Some(GitStatus::NotARepo);
    let v = view(&s, &MenuUiState::default());
    assert_eq!(v.empty.primary, "not a git repo");
    s.git.status = Some(GitStatus::GitMissing);
    let v = view(&s, &MenuUiState::default());
    assert_eq!(v.empty.primary, "git is not installed");
    // A dead filter is honest too — not a repo lie.
    s.git.status = Some(GitStatus::Ready);
    let ui = MenuUiState {
        query: "zzz".to_string(),
        ..MenuUiState::default()
    };
    let v = view(&s, &ui);
    assert_eq!(v.empty.primary, "no matches");
}

#[test]
fn the_filter_matches_subject_author_and_full_hash() {
    let mut s = state();
    s.git.commits = vec![
        commit(&h('a'), &[], "feat: alpha", vec![]),
        commit(&h('b'), &[], "fix: beta", vec![]),
    ];
    s.git.commits[1].author = "Grace".to_string();
    s.git.status = Some(GitStatus::Ready);
    let mut v = view(&s, &MenuUiState::default());
    v.apply_filter("beta");
    assert_eq!(v.rows.len(), 1, "subject match");
    let mut v = view(&s, &MenuUiState::default());
    v.apply_filter("grace");
    assert_eq!(v.rows.len(), 1, "author match via the row and search");
    // The full hash is findable though the row shows only the short one.
    let mut v = view(&s, &MenuUiState::default());
    v.apply_filter(&h('a')[..20]);
    assert_eq!(v.rows.len(), 1, "full-hash match");
    assert_eq!(
        v.rows[0].id().map(|i| i.as_str()),
        Some(format!("commit:{}", h('a')).as_str())
    );
}

#[test]
fn loading_more_owns_the_status_line() {
    let mut s = state();
    s.git.commits = vec![commit(&h('a'), &[], "one", vec![])];
    s.git.status = Some(GitStatus::Ready);
    s.git.has_more = true;
    s.git.begin_log(Some(&h('a')));
    let v = view(&s, &MenuUiState::default());
    assert_eq!(
        v.status.as_ref().map(|s| s.text.as_str()),
        Some("loading more")
    );
}

#[test]
fn on_open_fetches_only_when_nothing_is_known() {
    let mut s = state();
    assert_eq!(
        on_open(&s),
        vec![Action::GitLog { after: None }],
        "no reply ever landed — ask"
    );
    s.git.status = Some(GitStatus::NotARepo);
    assert!(on_open(&s).is_empty(), "a known status is an answer");
    s.git.status = None;
    s.git.loading = true;
    assert!(on_open(&s).is_empty(), "a request is already in flight");
}
