//! Named tool registry with per-agent subsets, plus the file primitives the
//! writing tools share: per-path write locks and atomic temp+rename writes.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use pacode_types::{AgentId, SessionId, ToolDefinition};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolError, ToolKind};

/// Ceiling on one agent's in-flight tool calls. The only remaining worker cap:
/// it bounds a single agent's fd/process/output footprint without throttling the
/// session, so 128 agents can each run this many calls at once. There is
/// deliberately no process-wide counterpart — a global cap would make a fleet of
/// agents queue behind each other, and the parent's own `agent spawn` call would
/// queue behind the children it just spawned.
pub const MAX_TOOL_WORKERS_PER_AGENT: usize = 8;
const OUTPUT_TRUNCATION_MARKER: &str = "\n\n[... output truncated to hard byte cap ...]\n\n";

pub const MAX_TOOL_OUTPUT_BYTES: usize = 512 * 1024;

/// Largest file a writing tool loads whole to diff against, and the largest
/// block a permission prompt carries.
pub const MAX_SOURCE_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PERMISSION_DETAIL_BYTES: usize = 64 * 1024;

/// How many of the first bytes decide "this is binary".
const BINARY_SNIFF_BYTES: usize = 8192;

/// Distinct paths holding a live write lock before the oldest entry is dropped.
/// Dropping only the map entry is safe: the owner keeps its own `Arc`.
const MAX_TRACKED_WRITE_PATHS: usize = 4096;

type AgentSemaphoreMap = HashMap<(SessionId, AgentId), Weak<Semaphore>>;
type PathWriteLockMap = HashMap<PathBuf, (Weak<AsyncMutex<()>>, u64)>;

static AGENT_TOOL_WORKER_SEMAPHORES: LazyLock<Mutex<AgentSemaphoreMap>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PATH_WRITE_LOCKS: LazyLock<Mutex<PathWriteLockMap>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static WRITE_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Held for the whole read-modify-write cycle of one path, so two writers
/// cannot interleave their reads and lose one of the two writes.
pub struct PathWriteGuard {
    _lock: Arc<AsyncMutex<()>>,
    _guard: OwnedMutexGuard<()>,
}

/// Why a whole-file read was refused.
#[derive(Debug)]
pub enum SourceReadError {
    Io(std::io::Error),
    NotRegularFile,
    TooLarge { len: u64, max: u64 },
    Binary,
    NotUtf8(std::str::Utf8Error),
}

impl SourceReadError {
    /// Model-facing text; `verb` is the tool's action (`edit`, `overwrite`),
    /// `label` the path as the model wrote it.
    pub fn message(&self, verb: &str, label: &str) -> String {
        match self {
            Self::Io(err) => format!("io: {err}"),
            Self::NotRegularFile => format!("cannot {verb} {label}: not a regular file"),
            Self::TooLarge { len, max } => format!(
                "cannot {verb} {label}: {len} bytes is over the {max} byte limit on a file \
                 loaded whole; narrow the change with `bash` instead"
            ),
            Self::Binary => format!("cannot {verb} binary file: {label}"),
            Self::NotUtf8(err) => format!("file is not valid UTF-8: {err}"),
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(err) if err.kind() == std::io::ErrorKind::NotFound)
    }
}

#[derive(Clone, Default)]
struct RegistryState {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    disabled: HashSet<String>,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    state: Arc<RegistryState>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Self {
        self.clone()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let state = Arc::make_mut(&mut self.state);
        if state.disabled.contains(tool.name()) {
            log::debug!(
                "tool '{}' is disabled via config; not registered",
                tool.name()
            );
            return;
        }
        state.tools.insert(tool.name().to_string(), tool);
    }

    /// Drop registered tools whose names are disabled and refuse future
    /// registrations of those names. Called once with `Config::disabled_tool_names`
    /// when the daemon wires the shared registry; clones and subsets keep the set.
    pub fn apply_disabled(&mut self, names: impl IntoIterator<Item = String>) {
        let set: HashSet<String> = names.into_iter().collect();
        if set.is_empty() {
            return;
        }
        let state = Arc::make_mut(&mut self.state);
        state.tools.retain(|name, _| !set.contains(name));
        state.disabled = set;
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.state.tools.get(name).cloned()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.state.tools.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.state.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.tools.is_empty()
    }

    /// Definitions for the model, in name order (stable for prompt caching).
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.state
            .tools
            .values()
            .map(|tool| tool.definition())
            .collect()
    }

    /// A registry restricted to `names` (unknown names ignored).
    pub fn subset<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> ToolRegistry {
        let mut out = ToolRegistry {
            state: Arc::new(RegistryState {
                tools: BTreeMap::new(),
                disabled: self.state.disabled.clone(),
            }),
        };
        for name in names {
            if let Some(tool) = self.get(name) {
                out.register(tool);
            }
        }
        out
    }

    /// Only tools of the given kinds (e.g. read-only set for Plan mode or subagents).
    pub fn filter_kinds(&self, kinds: &[ToolKind]) -> ToolRegistry {
        let mut out = ToolRegistry {
            state: Arc::new(RegistryState {
                tools: BTreeMap::new(),
                disabled: self.state.disabled.clone(),
            }),
        };
        for tool in self.state.tools.values() {
            if kinds.contains(&tool.kind()) {
                out.register(tool.clone());
            }
        }
        out
    }

    /// Take one of this agent's `MAX_TOOL_WORKERS_PER_AGENT` slots, waiting for
    /// a free one or for `cancel`. The permit lives as long as the returned
    /// guard, so callers must hold it across the whole call. Nothing here is
    /// process-wide: agents do not queue behind each other.
    pub async fn acquire_worker(
        &self,
        session: &SessionId,
        agent: &AgentId,
        cancel: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, ToolError> {
        let key = (session.clone(), agent.clone());
        let agent_semaphore = {
            let mut agents = AGENT_TOOL_WORKER_SEMAPHORES
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            agents.retain(|_, semaphore| semaphore.strong_count() > 0);
            if let Some(semaphore) = agents.get(&key).and_then(Weak::upgrade) {
                semaphore
            } else {
                let semaphore = Arc::new(Semaphore::new(MAX_TOOL_WORKERS_PER_AGENT));
                agents.insert(key, Arc::downgrade(&semaphore));
                semaphore
            }
        };

        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ToolError::Cancelled),
            permit = agent_semaphore.acquire_owned() => {
                permit.map_err(|_| ToolError::failed("per-agent tool worker pool closed"))
            }
        }
    }
}

/// Serialise the read-modify-write cycle of `path`. The key is the resolved
/// path, so `a/b`, `./a/b` and a symlinked parent share one lock; the lock is
/// held for as long as the caller's guard lives, which for the writing tools
/// spans the permission prompt — a second writer waits instead of writing a
/// stale copy over the first one's change.
pub async fn acquire_path_write_lock(
    path: &Path,
    cancel: &CancellationToken,
) -> Result<PathWriteGuard, ToolError> {
    let key = crate::builtin::helpers::canonicalize_best_effort(path);
    let lock = {
        let mut locks = PATH_WRITE_LOCKS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, (lock, _)| lock.strong_count() > 0);
        match locks.get(&key).and_then(|(lock, _)| lock.upgrade()) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(key, (Arc::downgrade(&lock), next_write_path_seq()));
                lock
            }
        }
    };
    evict_oldest_write_path();

    let guard = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ToolError::Cancelled),
        guard = Arc::clone(&lock).lock_owned() => guard,
    };
    Ok(PathWriteGuard {
        _lock: lock,
        _guard: guard,
    })
}

/// Read `path` whole, refusing anything that cannot be turned into text or is
/// too large to hold: a fifo or device would block on open forever, and a huge
/// file is a diff and memory hazard rather than an edit.
pub async fn read_source_file(path: &Path, max_bytes: u64) -> Result<String, SourceReadError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(SourceReadError::Io)?;
    if !meta.is_file() {
        return Err(SourceReadError::NotRegularFile);
    }
    if meta.len() > max_bytes {
        return Err(SourceReadError::TooLarge {
            len: meta.len(),
            max: max_bytes,
        });
    }
    let bytes = tokio::fs::read(path).await.map_err(SourceReadError::Io)?;
    let sniff = bytes.len().min(BINARY_SNIFF_BYTES);
    if bytes[..sniff].contains(&0) {
        return Err(SourceReadError::Binary);
    }
    String::from_utf8(bytes).map_err(|err| SourceReadError::NotUtf8(err.utf8_error()))
}

/// Write `bytes` to `target` through a sibling temp file and a rename: a reader
/// never sees a half-written file, a failed write leaves the old content in
/// place, and the temp is removed instead of left behind. The temp is hidden
/// and unique per process and call, so a rename can never pick up another
/// writer's bytes and the file indexer never sees it mid-write. An existing
/// target's mode is carried over — a plain create would drop an executable bit.
pub async fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), ToolError> {
    let temp = temp_path_for(target);
    let written = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        if let Ok(meta) = tokio::fs::metadata(target).await
            && let Err(err) = file.set_permissions(meta.permissions()).await
        {
            log::warn!(
                "atomic write of {} could not carry over the existing mode: {err}",
                target.display()
            );
        }
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, target).await
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    written.map_err(ToolError::Io)
}

fn temp_path_for(target: &Path) -> PathBuf {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let seq = next_write_path_seq();
    dir.join(format!(".{name}.pacode-tmp-{}-{}", std::process::id(), seq))
}

fn next_write_path_seq() -> u64 {
    WRITE_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

fn evict_oldest_write_path() {
    let mut locks = PATH_WRITE_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while locks.len() > MAX_TRACKED_WRITE_PATHS {
        let Some(oldest) = locks
            .iter()
            .min_by_key(|(_, (_, seq))| *seq)
            .map(|(path, _)| path.clone())
        else {
            break;
        };
        locks.remove(&oldest);
    }
}

pub fn cap_output_bytes<'a>(text: impl Into<Cow<'a, str>>, max_bytes: usize) -> Cow<'a, str> {
    let text = text.into();
    if text.len() <= max_bytes {
        return text;
    }
    if max_bytes == 0 {
        return Cow::Owned(String::new());
    }
    if max_bytes <= OUTPUT_TRUNCATION_MARKER.len() {
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return Cow::Owned(text[..end].to_string());
    }

    let content_bytes = max_bytes - OUTPUT_TRUNCATION_MARKER.len();
    let head_bytes = content_bytes * 2 / 3;
    let tail_bytes = content_bytes - head_bytes;
    let mut head_end = head_bytes.min(text.len());
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(tail_bytes);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if head_end > tail_start {
        return Cow::Owned(text[..head_end].to_string());
    }

    let mut out = String::with_capacity(max_bytes);
    out.push_str(&text[..head_end]);
    out.push_str(OUTPUT_TRUNCATION_MARKER);
    out.push_str(&text[tail_start..]);
    Cow::Owned(out)
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
