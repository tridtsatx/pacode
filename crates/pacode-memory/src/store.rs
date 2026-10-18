//! `MemoryStore`: global + project memory behind two mutex-guarded SQLite connections.
//!
//! Concurrency: one `Mutex<Option<ScopeDb>>` per scope, opened lazily on first use
//! (never on a timer, no background threads). WAL + `busy_timeout` make writers from
//! other processes (parallel sessions) safe; the mutex serialises the agents of this
//! process. Typical operations take well under 5 ms. Calls are synchronous: async callers
//! should run them on a blocking thread.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::TransactionBehavior;

use crate::db::{ScopeDb, legacy_path};
use crate::embed::Embedder;
use crate::error::MemoryError;
use crate::hybrid::{self, QueryVec};
use crate::id::new_id;
use crate::inject::{PINNED_CAP, RECENT_CAP, ScopeSnapshot, Snapshot};
use crate::text::fts_query;
use crate::types::{
    Entry, Hit, Kind, MAX_RECALL_LIMIT, NewEntry, RecallRequest, Scope, WriteOutcome,
};
use crate::{ops, rank};

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Paths of the two databases for a working directory.
pub fn project_db_path(cwd: &Path) -> PathBuf {
    cwd.join(".pacode").join("memory.db")
}

pub fn global_db_path(config_dir: &Path) -> PathBuf {
    config_dir.join("memory.db")
}

pub struct MemoryStore {
    paths: [PathBuf; 2],
    dbs: [Mutex<Option<ScopeDb>>; 2],
    embedder: Mutex<Option<Arc<dyn Embedder>>>,
    clock: Clock,
}

impl MemoryStore {
    pub fn new(global_db: PathBuf, project_db: PathBuf) -> Self {
        Self {
            paths: [global_db, project_db],
            dbs: [Mutex::new(None), Mutex::new(None)],
            embedder: Mutex::new(None),
            clock: Box::new(system_now_ms),
        }
    }

    /// Store for `<config_dir>/memory.db` and `<cwd>/.pacode/memory.db`.
    pub fn for_dirs(config_dir: &Path, cwd: &Path) -> Self {
        Self::new(global_db_path(config_dir), project_db_path(cwd))
    }

    /// Replace the clock (tests).
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Enable the semantic layer. Callers do this only when `[memory] embeddings = true`.
    pub fn set_embedder(&self, embedder: Option<Arc<dyn Embedder>>) {
        *lock(&self.embedder) = embedder;
    }

    fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        lock(&self.embedder).clone()
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn idx(scope: Scope) -> usize {
        match scope {
            Scope::Global => 0,
            Scope::Project => 1,
        }
    }

    /// Run `f` on the scope database, opening it if needed. With `create == false`
    /// nothing is created: `Ok(None)` when neither the database nor a legacy
    /// `memory.md` exists yet.
    fn with_db<T>(
        &self,
        scope: Scope,
        create: bool,
        f: impl FnOnce(&mut ScopeDb) -> Result<T, MemoryError>,
    ) -> Result<Option<T>, MemoryError> {
        let i = Self::idx(scope);
        let mut guard = lock(&self.dbs[i]);
        if guard.is_none() {
            let path = &self.paths[i];
            if !create && !path.exists() && !legacy_path(path).exists() {
                return Ok(None);
            }
            *guard = Some(ScopeDb::open(path, self.now())?);
        }
        match guard.as_mut() {
            Some(db) => f(db).map(Some),
            None => Ok(None),
        }
    }

    /// Store a memory, or update the near-duplicate already there.
    pub fn write(&self, scope: Scope, entry: NewEntry) -> Result<WriteOutcome, MemoryError> {
        let n = entry.normalize()?;
        let now = self.now();
        let out = self.with_db(scope, true, |db| {
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (id, created) = match ops::find_duplicate(&tx, scope, &n)? {
                Some(id) => {
                    ops::update(&tx, &id, &n, now)?;
                    (id, false)
                }
                None => {
                    let id = new_id(now);
                    ops::insert(&tx, &id, &n, now)?;
                    (id, true)
                }
            };
            let superseded = match &n.supersedes {
                Some(old) => ops::supersede(&tx, old, &id, now)?,
                None => false,
            };
            tx.commit()?;
            Ok(WriteOutcome {
                id,
                scope,
                created,
                superseded,
            })
        })?;
        let out = out.ok_or_else(|| MemoryError::invalid("memory database unavailable"))?;
        self.embed_entry(scope, &out.id, &n);
        Ok(out)
    }

    /// Best effort: a failing embedder never fails the write.
    fn embed_entry(&self, scope: Scope, id: &str, n: &NewEntry) {
        let Some(emb) = self.embedder() else { return };
        let input = vec![format!("{}\n{}", n.title, n.text)];
        let vec = match emb.embed(&input) {
            Ok(mut v) if v.len() == 1 && v[0].len() == emb.dim() => v.remove(0),
            Ok(_) => {
                log::warn!("embedder '{}' returned an unexpected shape", emb.id());
                return;
            }
            Err(e) => {
                log::warn!("embedding failed: {e}");
                return;
            }
        };
        let res = self.with_db(scope, true, |db| {
            hybrid::upsert_embedding(&db.conn, id, emb.id(), &vec)
        });
        if let Err(e) = res {
            log::warn!("storing embedding failed: {e}");
        }
    }

    /// Ranked recall across the requested scopes. Bumps `last_recalled_ms` and
    /// `recall_count` of the returned entries. A blank query lists pinned + recent.
    pub fn recall(&self, req: &RecallRequest) -> Result<Vec<Hit>, MemoryError> {
        let limit = req.limit.clamp(1, MAX_RECALL_LIMIT);
        let now = self.now();
        let fts = fts_query(&req.query);
        let embedder = self.embedder();
        // Query embedding happens before any database lock is taken.
        let qvec: Option<Vec<f32>> = match (&fts, &embedder) {
            (Some(_), Some(emb)) => match emb.embed(std::slice::from_ref(&req.query)) {
                Ok(mut v) if v.len() == 1 && v[0].len() == emb.dim() => Some(v.remove(0)),
                Ok(_) => None,
                Err(e) => {
                    log::warn!("query embedding failed, falling back to FTS: {e}");
                    None
                }
            },
            _ => None,
        };
        let mut scopes: Vec<Scope> = Vec::new();
        for s in &req.scopes {
            if !scopes.contains(s) {
                scopes.push(*s);
            }
        }
        let mut hits: Vec<Hit> = Vec::new();
        for scope in scopes {
            let part = self.with_db(scope, false, |db| match &fts {
                Some(q) => {
                    let qv = match (&qvec, &embedder) {
                        (Some(v), Some(emb)) => Some(QueryVec {
                            vector: v,
                            embedder_id: emb.id(),
                        }),
                        _ => None,
                    };
                    hybrid::rank_scope(&db.conn, scope, q, req.kind, qv, now)
                }
                None => Ok(ops::recent(&db.conn, scope, req.kind, limit)?
                    .into_iter()
                    .map(|entry| Hit { entry, score: 0.0 })
                    .collect()),
            })?;
            hits.extend(part.unwrap_or_default());
        }
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| b.entry.pinned.cmp(&a.entry.pinned))
                .then_with(|| b.entry.updated_ms.cmp(&a.entry.updated_ms))
                .then_with(|| a.entry.id.cmp(&b.entry.id))
        });
        hits.truncate(limit);
        for scope in Scope::ALL {
            let ids: Vec<String> = hits
                .iter()
                .filter(|h| h.entry.scope == scope)
                .map(|h| h.entry.id.clone())
                .collect();
            if !ids.is_empty() {
                self.with_db(scope, false, |db| ops::touch(&db.conn, &ids, now))?;
            }
        }
        Ok(hits)
    }

    /// Soft-delete a live entry (hard-deleted after 90 days at a later store open).
    /// Looks in both scopes; returns the forgotten entry.
    pub fn forget(&self, id: &str) -> Result<Entry, MemoryError> {
        let now = self.now();
        for scope in Scope::ALL {
            let found = self.with_db(scope, false, |db| {
                match ops::get_live(&db.conn, scope, id)? {
                    Some(e) if ops::soft_delete(&db.conn, id, now)? => Ok(Some(e)),
                    _ => Ok(None),
                }
            })?;
            if let Some(Some(e)) = found {
                return Ok(e);
            }
        }
        Err(MemoryError::NotFound(id.to_string()))
    }

    /// Entries for listing: pinned first, then most recently updated; merged across
    /// scopes and capped at `limit`.
    pub fn list(
        &self,
        scopes: &[Scope],
        kind: Option<Kind>,
        limit: usize,
    ) -> Result<Vec<Entry>, MemoryError> {
        let limit = limit.clamp(1, 200);
        let mut out: Vec<Entry> = Vec::new();
        for scope in Scope::ALL.into_iter().filter(|s| scopes.contains(s)) {
            let part =
                self.with_db(scope, false, |db| ops::recent(&db.conn, scope, kind, limit))?;
            out.extend(part.unwrap_or_default());
        }
        out.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| b.updated_ms.cmp(&a.updated_ms))
                .then_with(|| a.id.cmp(&b.id))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Prompt snapshot (see [`crate::inject`]). Scopes without a database (or whose
    /// database cannot be opened) are empty; failures are logged, never raised.
    pub fn snapshot(&self) -> Snapshot {
        let mut snap = Snapshot::default();
        for scope in Scope::ALL {
            let res = self.with_db(scope, false, |db| {
                let (pinned, recent) = ops::snapshot(&db.conn, scope, PINNED_CAP, RECENT_CAP)?;
                Ok(ScopeSnapshot { pinned, recent })
            });
            let part = match res {
                Ok(p) => p.unwrap_or_default(),
                Err(e) => {
                    log::warn!("memory snapshot ({}) failed: {e}", scope.as_str());
                    ScopeSnapshot::default()
                }
            };
            match scope {
                Scope::Global => snap.global = part,
                Scope::Project => snap.project = part,
            }
        }
        snap
    }

    /// Ranking boost of an entry at the store's current time (for diagnostics).
    pub fn boost_of(&self, e: &Entry) -> f64 {
        rank::boost(e, self.now())
    }
}
