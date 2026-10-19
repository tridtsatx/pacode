//! Process-wide prompt cache shared by every agent of a cwd (perf C3).
//!
//! Owner: this module. Cap: [`MAX_CWDS`] entries (least recently used is dropped).
//! Invalidation: a main-agent turn start calls [`SharedPromptCache::invalidate`] for its
//! cwd (drops the entries and bumps the generation, so a build that started earlier is
//! not stored), and every entry expires after [`TTL`]. Consequence: instruction, branch
//! and memory edits reach subagents at the next main turn or within `TTL`, whichever
//! comes first. Builds (filesystem and memory DB reads) run under `spawn_blocking`; the
//! lock is never held across an await.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::TurnPromptCache;

/// Distinct (cwd, options) entries kept.
pub const MAX_CWDS: usize = 16;
/// Entry lifetime for non-main agents.
pub const TTL: Duration = Duration::from_secs(5);

/// Everything besides the cwd that shapes a [`TurnPromptCache`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptOpts {
    pub home_config: Option<PathBuf>,
    pub instructions_cap_chars: usize,
    pub memory_cap_chars: usize,
    pub discover_foreign: bool,
}

struct Entry {
    cwd: PathBuf,
    opts: PromptOpts,
    built: Instant,
    value: Arc<TurnPromptCache>,
}

struct Inner {
    generation: u64,
    /// Least recently used first.
    entries: Vec<Entry>,
}

pub struct SharedPromptCache {
    inner: Mutex<Inner>,
}

impl SharedPromptCache {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                generation: 0,
                entries: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drop every entry of `cwd` and make in-flight builds unstorable.
    pub fn invalidate(&self, cwd: &Path) {
        let mut g = self.lock();
        g.generation += 1;
        g.entries.retain(|e| e.cwd != cwd);
    }

    /// A fresh (younger than [`TTL`]) entry, marked most recently used.
    pub fn get(&self, cwd: &Path, opts: &PromptOpts, now: Instant) -> Option<Arc<TurnPromptCache>> {
        let mut g = self.lock();
        let idx = g.entries.iter().position(|e| {
            e.cwd == cwd && &e.opts == opts && now.saturating_duration_since(e.built) < TTL
        })?;
        let entry = g.entries.remove(idx);
        let value = entry.value.clone();
        g.entries.push(entry);
        Some(value)
    }

    /// Store `value` built at `now` unless an invalidation happened since `generation`
    /// was read. A fresh entry that appeared meanwhile wins, so concurrent subagents
    /// converge on one `Arc`.
    fn put(
        &self,
        cwd: &Path,
        opts: PromptOpts,
        generation: u64,
        now: Instant,
        value: Arc<TurnPromptCache>,
    ) -> Arc<TurnPromptCache> {
        let mut g = self.lock();
        if g.generation != generation {
            return value;
        }
        g.entries.retain(|e| {
            !(e.cwd == cwd && e.opts == opts) || now.saturating_duration_since(e.built) < TTL
        });
        if let Some(existing) = g
            .entries
            .iter()
            .find(|e| e.cwd == cwd && e.opts == opts)
            .map(|e| e.value.clone())
        {
            return existing;
        }
        g.entries.push(Entry {
            cwd: cwd.to_path_buf(),
            opts,
            built: now,
            value: value.clone(),
        });
        while g.entries.len() > MAX_CWDS {
            g.entries.remove(0);
        }
        value
    }

    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Main agents invalidate and rebuild; others reuse a fresh entry or rebuild.
    pub async fn resolve(
        &self,
        cwd: &Path,
        opts: PromptOpts,
        is_main: bool,
        now: Instant,
    ) -> Arc<TurnPromptCache> {
        if is_main {
            self.invalidate(cwd);
        } else if let Some(hit) = self.get(cwd, &opts, now) {
            return hit;
        }
        let generation = self.generation();
        let (c, o) = (cwd.to_path_buf(), opts.clone());
        let built = tokio::task::spawn_blocking(move || {
            TurnPromptCache::with_options(
                &c,
                o.home_config.as_deref(),
                o.instructions_cap_chars,
                o.memory_cap_chars,
                o.discover_foreign,
            )
        })
        .await
        .unwrap_or_else(|_| TurnPromptCache::empty());
        self.put(cwd, opts, generation, now, Arc::new(built))
    }
}

impl Default for SharedPromptCache {
    fn default() -> Self {
        Self::new()
    }
}

static SHARED: SharedPromptCache = SharedPromptCache::new();

/// The process-wide cache entry for `cwd` (see the module docs for caps/invalidation).
pub async fn shared(cwd: &Path, opts: PromptOpts, is_main: bool) -> Arc<TurnPromptCache> {
    SHARED.resolve(cwd, opts, is_main, Instant::now()).await
}
