use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tempfile::TempDir;

use crate::inject::{GUIDANCE, render};
use crate::legacy::migrated_path;
use crate::*;

const DAY: i64 = 86_400_000;

struct Fixture {
    dir: TempDir,
    now: Arc<AtomicI64>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
            now: Arc::new(AtomicI64::new(1_700_000_000_000)),
        }
    }
    fn store(&self) -> MemoryStore {
        let now = self.now.clone();
        MemoryStore::for_dirs(&self.global_dir(), &self.cwd())
            .with_clock(move || now.load(Ordering::SeqCst))
    }
    fn global_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("cfg")
    }
    fn cwd(&self) -> std::path::PathBuf {
        self.dir.path().join("proj")
    }
    fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

fn entry(kind: Kind, title: &str, text: &str) -> NewEntry {
    NewEntry::new(kind, title, text)
}

fn titles(hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|h| h.entry.title.clone()).collect()
}

#[test]
fn write_recall_forget_round_trip() {
    let fx = Fixture::new();
    let s = fx.store();
    let out = s
        .write(
            Scope::Global,
            entry(Kind::Fact, "Build command", "run cargo build --release"),
        )
        .unwrap();
    assert!(out.created);
    let hits = s.recall(&RecallRequest::new("cargo")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.id, out.id);
    assert_eq!(hits[0].entry.scope, Scope::Global);
    assert_eq!(hits[0].entry.recall_count, 0);
    // The first recall bumped the counter.
    let again = s.recall(&RecallRequest::new("cargo")).unwrap();
    assert_eq!(again[0].entry.recall_count, 1);
    assert!(again[0].entry.last_recalled_ms > 0);

    let gone = s.forget(&out.id).unwrap();
    assert_eq!(gone.title, "Build command");
    assert!(s.recall(&RecallRequest::new("cargo")).unwrap().is_empty());
    assert!(matches!(s.forget(&out.id), Err(MemoryError::NotFound(_))));
}

#[test]
fn same_title_updates_instead_of_inserting() {
    let fx = Fixture::new();
    let s = fx.store();
    let a = s
        .write(
            Scope::Project,
            entry(Kind::Fact, "Test runner", "uses nextest"),
        )
        .unwrap();
    fx.advance(1000);
    let b = s
        .write(
            Scope::Project,
            entry(Kind::Decision, "  test   RUNNER ", "uses cargo test now"),
        )
        .unwrap();
    assert!(!b.created);
    assert_eq!(a.id, b.id);
    let all = s.list(&[Scope::Project], None, 50).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].text, "uses cargo test now");
    assert_eq!(all[0].kind, Kind::Decision);
    assert!(all[0].updated_ms > all[0].created_ms);
}

#[test]
fn near_duplicate_text_updates_existing() {
    let fx = Fixture::new();
    let s = fx.store();
    let a = s
        .write(
            Scope::Global,
            entry(
                Kind::Preference,
                "Indentation style",
                "always use tabs for indentation in go files please",
            ),
        )
        .unwrap();
    let b = s
        .write(
            Scope::Global,
            entry(
                Kind::Preference,
                "Go indent",
                "always use tabs for indentation in go files please",
            ),
        )
        .unwrap();
    assert!(!b.created);
    assert_eq!(a.id, b.id);
    let c = s
        .write(
            Scope::Global,
            entry(Kind::Fact, "Deploy target", "staging cluster runs on helm"),
        )
        .unwrap();
    assert!(c.created);
}

#[test]
fn pinned_and_recency_boost_ranking() {
    let fx = Fixture::new();
    let s = fx.store();
    let body = "deploy the staging cluster with helm";
    for (i, t) in ["Filler one", "Filler two", "Filler three"]
        .iter()
        .enumerate()
    {
        s.write(
            Scope::Global,
            entry(
                Kind::Fact,
                t,
                &format!("unrelated words number{i} zebra{i} quartz{i}"),
            ),
        )
        .unwrap();
    }
    s.write(Scope::Global, entry(Kind::Fact, "Alpha runbook", body))
        .unwrap();
    let mut pinned = entry(Kind::Fact, "Beta checklist", body);
    pinned.pinned = true;
    s.write(Scope::Global, pinned).unwrap();
    let hits = s.recall(&RecallRequest::new("deploy helm")).unwrap();
    assert_eq!(titles(&hits), vec!["Beta checklist", "Alpha runbook"]);

    // Recency: same text, the newer entry wins when neither is pinned.
    let s2 = Fixture::new();
    let st = s2.store();
    for (i, t) in ["Filler one", "Filler two", "Filler three"]
        .iter()
        .enumerate()
    {
        st.write(
            Scope::Global,
            entry(
                Kind::Fact,
                t,
                &format!("unrelated words number{i} zebra{i} quartz{i}"),
            ),
        )
        .unwrap();
    }
    st.write(Scope::Global, entry(Kind::Fact, "Gamma runbook", body))
        .unwrap();
    s2.advance(200 * DAY);
    st.write(Scope::Global, entry(Kind::Fact, "Delta checklist", body))
        .unwrap();
    let hits = st.recall(&RecallRequest::new("deploy helm")).unwrap();
    assert_eq!(titles(&hits), vec!["Delta checklist", "Gamma runbook"]);
}

#[test]
fn cyrillic_query_matches_case_insensitively_and_by_prefix() {
    let fx = Fixture::new();
    let s = fx.store();
    s.write(
        Scope::Project,
        entry(
            Kind::Fact,
            "Тест-фреймворк",
            "Проект использует pytest для интеграционных тестов",
        ),
    )
    .unwrap();
    s.write(
        Scope::Project,
        entry(Kind::Fact, "Деплой", "выкладываем через helm в staging"),
    )
    .unwrap();
    let hits = s.recall(&RecallRequest::new("ИНТЕГРАЦИОННЫХ")).unwrap();
    assert_eq!(titles(&hits), vec!["Тест-фреймворк"]);
    let hits = s.recall(&RecallRequest::new("интеграц")).unwrap();
    assert_eq!(hits.len(), 1);
    let hits = s.recall(&RecallRequest::new("деплой")).unwrap();
    assert_eq!(titles(&hits), vec!["Деплой"]);
}

#[test]
fn legacy_memory_md_is_imported_once_and_renamed() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.global_dir()).unwrap();
    let mut md = String::from("# header ignored\n");
    for i in 0..25 {
        md.push_str(&format!("- global note {i}\n"));
    }
    let md_path = fx.global_dir().join("memory.md");
    std::fs::write(&md_path, md).unwrap();
    std::fs::create_dir_all(fx.cwd().join(".pacode")).unwrap();
    std::fs::write(fx.cwd().join(".pacode").join("memory.md"), "- proj note\n").unwrap();

    let s = fx.store();
    let snap = s.snapshot();
    assert_eq!(snap.global.pinned.len(), 20);
    assert_eq!(snap.global.recent.len(), 5);
    assert_eq!(snap.global.pinned[0].text, "global note 0");
    assert_eq!(snap.global.pinned[0].kind, Kind::Fact);
    assert_eq!(snap.project.pinned.len(), 1);
    assert!(!md_path.exists());
    assert!(migrated_path(&md_path).exists());

    // Reopen: nothing is imported twice.
    let s2 = fx.store();
    assert_eq!(s2.list(&[Scope::Global], None, 200).unwrap().len(), 25);
}

#[test]
fn caps_are_enforced() {
    let fx = Fixture::new();
    let s = fx.store();
    let mut e = entry(Kind::Lesson, &"t".repeat(200), &"x".repeat(3000));
    e.tags = (0..12).map(|i| format!("Tag {i}")).collect();
    let out = s.write(Scope::Global, e).unwrap();
    let got = &s.list(&[Scope::Global], None, 5).unwrap()[0];
    assert_eq!(got.id, out.id);
    assert_eq!(got.title.chars().count(), 80);
    assert_eq!(got.text.chars().count(), 2000);
    assert_eq!(got.tags.len(), 8);
    assert_eq!(got.tags[0], "tag-0");
    assert!(matches!(
        s.write(Scope::Global, entry(Kind::Fact, "t", "   ")),
        Err(MemoryError::Invalid(_))
    ));

    for i in 0..25 {
        s.write(
            Scope::Project,
            entry(
                Kind::Fact,
                &format!("item {i}"),
                &format!("topic{i} alpha{i} beta{i} gamma{i}"),
            ),
        )
        .unwrap();
    }
    let mut req = RecallRequest::new("topic");
    req.limit = 100;
    assert_eq!(s.recall(&req).unwrap().len(), 20);
    req.limit = 0;
    assert_eq!(s.recall(&req).unwrap().len(), 1);
}

#[test]
fn global_and_project_are_merged_and_filterable() {
    let fx = Fixture::new();
    let s = fx.store();
    s.write(
        Scope::Global,
        entry(
            Kind::Preference,
            "Editor preference",
            "likes vim keybindings in the editor",
        ),
    )
    .unwrap();
    s.write(
        Scope::Project,
        entry(
            Kind::Gotcha,
            "Editor config",
            "the editor plugin crashes on save",
        ),
    )
    .unwrap();
    let both = s.recall(&RecallRequest::new("editor")).unwrap();
    assert_eq!(both.len(), 2);
    assert!(both.iter().any(|h| h.entry.scope == Scope::Global));
    assert!(both.iter().any(|h| h.entry.scope == Scope::Project));
    let mut only = RecallRequest::new("editor");
    only.scopes = vec![Scope::Project];
    assert_eq!(titles(&s.recall(&only).unwrap()), vec!["Editor config"]);
    let mut by_kind = RecallRequest::new("editor");
    by_kind.kind = Some(Kind::Preference);
    assert_eq!(
        titles(&s.recall(&by_kind).unwrap()),
        vec!["Editor preference"]
    );
    assert!(fx.cwd().join(".pacode").join("memory.db").exists());
    assert!(fx.global_dir().join("memory.db").exists());
}

#[test]
fn blank_query_lists_pinned_then_recent() {
    let fx = Fixture::new();
    let s = fx.store();
    s.write(Scope::Global, entry(Kind::Fact, "Old plain", "aaa one"))
        .unwrap();
    fx.advance(1000);
    let mut p = entry(Kind::Fact, "Pinned thing", "bbb two");
    p.pinned = true;
    s.write(Scope::Global, p).unwrap();
    fx.advance(1000);
    s.write(Scope::Global, entry(Kind::Fact, "New plain", "ccc three"))
        .unwrap();
    let hits = s.recall(&RecallRequest::new("")).unwrap();
    assert_eq!(
        titles(&hits),
        vec!["Pinned thing", "New plain", "Old plain"]
    );
}

#[test]
fn supersedes_hides_the_old_entry() {
    let fx = Fixture::new();
    let s = fx.store();
    let old = s
        .write(
            Scope::Project,
            entry(Kind::Decision, "Database choice", "we use postgres"),
        )
        .unwrap();
    let mut n = entry(Kind::Decision, "Storage engine", "we migrated to sqlite");
    n.supersedes = Some(old.id.clone());
    let new = s.write(Scope::Project, n).unwrap();
    assert!(new.superseded);
    let all = s.list(&[Scope::Project], None, 10).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, new.id);
}

#[test]
fn forgotten_entries_are_purged_after_90_days_at_open() {
    let fx = Fixture::new();
    let id = {
        let s = fx.store();
        let out = s
            .write(Scope::Global, entry(Kind::Fact, "Temp", "short lived"))
            .unwrap();
        s.forget(&out.id).unwrap();
        out.id
    };
    let count = |dir: &std::path::Path| -> i64 {
        let c = rusqlite::Connection::open(dir.join("memory.db")).unwrap();
        c.query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE id = ?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap()
    };
    fx.advance(30 * DAY);
    drop(fx.store().list(&[Scope::Global], None, 1));
    assert_eq!(
        count(&fx.global_dir()),
        1,
        "still a tombstone after 30 days"
    );
    fx.advance(61 * DAY);
    drop(fx.store().list(&[Scope::Global], None, 1));
    assert_eq!(count(&fx.global_dir()), 0, "hard-deleted after 90 days");
}

#[test]
fn snapshot_render_is_deterministic_capped_and_has_guidance() {
    let fx = Fixture::new();
    let s = fx.store();
    assert!(render(&s.snapshot(), 8000).is_none());
    let mut p = entry(
        Kind::Preference,
        "Reply language",
        "always answer in Russian",
    );
    p.pinned = true;
    s.write(Scope::Global, p).unwrap();
    for i in 0..12 {
        fx.advance(1000);
        s.write(
            Scope::Project,
            entry(
                Kind::Fact,
                &format!("Recent item {i}"),
                &format!("uniq{i} word{i} other{i}"),
            ),
        )
        .unwrap();
    }
    let a = render(&s.snapshot(), 8000).unwrap();
    let b = render(&s.snapshot(), 8000).unwrap();
    assert_eq!(a, b);
    assert!(a.starts_with("# Memory (global)"));
    assert!(a.contains("always answer in Russian"));
    assert!(a.contains("Recent item 11"));
    assert!(a.contains("Recent item 2"));
    assert!(
        !a.contains("Recent item 1 ("),
        "only the 10 newest are indexed"
    );
    assert!(a.contains(GUIDANCE));
    let tight = render(&s.snapshot(), 400).unwrap();
    assert!(tight.chars().count() <= 400);
    let tiny = render(&s.snapshot(), 30).unwrap();
    assert!(tiny.chars().count() <= 30);
    assert!(render(&s.snapshot(), 0).is_none());
}

#[test]
fn concurrent_writers_are_safe() {
    let fx = Fixture::new();
    let s = Arc::new(fx.store());
    let mut handles = Vec::new();
    for t in 0..4 {
        let s = s.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..10 {
                s.write(
                    Scope::Project,
                    NewEntry::new(
                        Kind::Fact,
                        format!("t{t} n{i}"),
                        format!("thread{t}x{i} p{t}q{i} r{t}s{i}"),
                    ),
                )
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.list(&[Scope::Project], None, 200).unwrap().len(), 40);
}
