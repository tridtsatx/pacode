//! Disk cache for daemon list data (§8.2 cache-first).
//!
//! The state fills from this cache *before the first frame* (one blocking
//! read on `spawn_blocking`, never in the render loop), each daemon reply then
//! replaces its section wholesale and rewrites the file. While a refresh is
//! in flight the pickers show `loading…`; an entry past its TTL with no fresh
//! answer yet shows `offline copy`.
//!
//! Layout: `~/.cache/pacode/<key>.v<schema>.json`. `key` is fixed per list
//! except git, which is per-project (`git-<fingerprint>`) so switching
//! directories never shows one repo's history under another's. `schema`
//! bumps when a payload's shape changes — old files read as misses and are
//! swept by the mtime cap.

pub mod disk;

use std::borrow::Cow;
use std::path::Path;

use pacode_types::vcs::GitStatus;
use pacode_types::{MarketplacePluginInfo, McpServerInfo, ModelInfo, PluginInfo, SessionMeta};

use crate::state::AppState;

pub use disk::{CapsReport, DiskCache, Entry};

#[cfg(test)]
#[path = "cache_tests.rs"]
mod cache_tests;

/// The cacheable lists (§8.2). Each owns one `<key>.v<schema>.json` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    /// `ListModels` — model catalog.
    Models,
    /// `ListPlugins` — installed/loaded plugins.
    Plugins,
    /// `BrowseMarketplace` — the configured marketplace's index.
    Marketplace,
    /// `ListMcpServers`.
    McpServers,
    /// `ListSessions`.
    Sessions,
    /// The dynamic slash-command registry (plugin commands + MCP prompts).
    /// Written whenever plugins or MCP servers land; read at startup so `/x`
    /// works before the daemon answers.
    Commands,
    /// `GitLog` — last ≤200 commits of the session's project.
    GitLog,
}

impl ListKind {
    /// All kinds, for tests and sweeps.
    pub const ALL: [ListKind; 7] = [
        Self::Models,
        Self::Plugins,
        Self::Marketplace,
        Self::McpServers,
        Self::Sessions,
        Self::Commands,
        Self::GitLog,
    ];

    /// The `N` of `<key>.v<N>.json`. Bump when the payload shape changes.
    pub fn schema(self) -> u32 {
        1
    }

    /// The `key` of `<key>.v<N>.json`. Git history is project data, so its
    /// key carries a fingerprint of the session cwd — a per-project file is
    /// how §8.2's "project switch invalidates" is implemented here.
    pub fn stem(self, cwd: &Path) -> Cow<'static, str> {
        match self {
            Self::Models => "models".into(),
            Self::Plugins => "plugins".into(),
            Self::Marketplace => "marketplace".into(),
            Self::McpServers => "mcp".into(),
            Self::Sessions => "sessions".into(),
            Self::Commands => "commands".into(),
            Self::GitLog => format!("git-{}", cwd_fingerprint(cwd)).into(),
        }
    }

    /// Which list a `Request` refreshes, for freshness tracking and reply
    /// routing. `None` — not a list request.
    pub fn for_request(req: &pacode_types::Request) -> Option<Self> {
        use pacode_types::Request as R;
        match req {
            R::ListModels | R::RefreshModels => Some(Self::Models),
            R::ListPlugins => Some(Self::Plugins),
            R::BrowseMarketplace { .. } => Some(Self::Marketplace),
            R::ListMcpServers => Some(Self::McpServers),
            R::ListSessions { .. } => Some(Self::Sessions),
            _ => None,
        }
    }
}

/// Stable short fingerprint of a project path for the git cache key. FNV-1a —
/// deterministic across runs and toolchain versions (unlike `DefaultHasher`).
fn cwd_fingerprint(cwd: &Path) -> u64 {
    let bytes = cwd.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// How fresh a cached entry is for its kind (§8.2 TTL):
/// models → `provider.catalog_ttl_secs` (24 h default), marketplace → 1 h,
/// everything else → session-scoped: written before this client run started
/// means stale.
pub fn is_stale(
    kind: ListKind,
    written_at_ms: u64,
    now_ms: u64,
    session_start_ms: u64,
    catalog_ttl_secs: u64,
) -> bool {
    let age_ms = now_ms.saturating_sub(written_at_ms);
    match kind {
        ListKind::Models => age_ms > catalog_ttl_secs.saturating_mul(1000),
        ListKind::Marketplace => age_ms > 3_600_000,
        ListKind::Plugins
        | ListKind::McpServers
        | ListKind::Sessions
        | ListKind::Commands
        | ListKind::GitLog => written_at_ms < session_start_ms,
    }
}

/// Freshness of one cache-backed list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Freshness {
    /// Nothing on screen — no cache hit, no reply yet.
    #[default]
    Empty,
    /// A disk copy is on screen; `stale` when it is past its TTL.
    Cached { stale: bool },
    /// Daemon data landed this session.
    Fresh,
}

/// The badge a picker surfaces for one list (§8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// `loading…` — a request is in flight and nothing fresh has landed.
    Loading,
    /// `offline copy` — past-TTL disk data with no fresh answer yet.
    OfflineCopy,
}

/// Freshness + in-flight flag of one list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListHealth {
    pub freshness: Freshness,
    /// A refresh request is outstanding (set at dispatch, cleared at reply).
    pub in_flight: bool,
}

/// Per-list cache health. Owned by `AppState`; `app.rs` marks requests on
/// dispatch and replies on landing; `cache::apply_snapshot` fills `Cached`.
#[derive(Clone, Debug, Default)]
pub struct Lists {
    pub models: ListHealth,
    pub plugins: ListHealth,
    pub marketplace: ListHealth,
    pub mcp: ListHealth,
    pub sessions: ListHealth,
    pub git: ListHealth,
}

impl Lists {
    pub fn get(&self, kind: ListKind) -> &ListHealth {
        match kind {
            ListKind::Models => &self.models,
            ListKind::Plugins => &self.plugins,
            ListKind::Marketplace => &self.marketplace,
            ListKind::McpServers => &self.mcp,
            ListKind::Sessions => &self.sessions,
            ListKind::GitLog => &self.git,
            // The command registry has no list surface of its own.
            ListKind::Commands => &self.plugins,
        }
    }

    fn get_mut(&mut self, kind: ListKind) -> &mut ListHealth {
        match kind {
            ListKind::Models => &mut self.models,
            ListKind::Plugins | ListKind::Commands => &mut self.plugins,
            ListKind::Marketplace => &mut self.marketplace,
            ListKind::McpServers => &mut self.mcp,
            ListKind::Sessions => &mut self.sessions,
            ListKind::GitLog => &mut self.git,
        }
    }

    /// A refresh request went out.
    pub fn mark_sent(&mut self, kind: ListKind) {
        self.get_mut(kind).in_flight = true;
    }

    /// The reply landed. `ok` is false on transport errors and `Reply::Error`
    /// — the cached copy then stays what it was (a stale one keeps its
    /// `offline copy` badge).
    pub fn on_reply(&mut self, kind: ListKind, ok: bool) {
        let h = self.get_mut(kind);
        h.in_flight = false;
        if ok {
            h.freshness = Freshness::Fresh;
        }
    }

    /// A disk entry was applied at startup.
    pub fn apply_cached(&mut self, kind: ListKind, stale: bool) {
        self.get_mut(kind).freshness = Freshness::Cached { stale };
    }

    /// What a picker should badge this list with right now. A stale copy
    /// outranks an in-flight refresh: "offline copy" says the data on screen
    /// is old; "loading…" alone would suggest fresh data is already coming.
    pub fn badge(&self, kind: ListKind) -> Option<Badge> {
        let h = self.get(kind);
        if matches!(h.freshness, Freshness::Cached { stale: true }) {
            return Some(Badge::OfflineCopy);
        }
        if h.in_flight {
            return Some(Badge::Loading);
        }
        None
    }
}

/// Everything `startup_load` needs to decide which entries apply — owned so
/// the load can run inside `spawn_blocking` before the first frame.
#[derive(Clone, Debug)]
pub struct LoadCtx {
    /// Session cwd — keys the per-project git file.
    pub cwd: std::path::PathBuf,
    /// `mcp_sig` of the current config — a mismatched MCP entry is a miss.
    pub mcp_sig: String,
    /// `config.plugins.marketplace` — a listing for another source is a miss.
    pub marketplace_source: String,
}

/// Signature over the MCP server configuration — an edit invalidates the
/// cached server list (§8.2).
pub fn mcp_sig(config: &pacode_types::Config) -> String {
    serde_json::to_string(&config.mcp.servers).unwrap_or_default()
}

/// MCP list payload: the config signature it was fetched under, for free
/// invalidation on config change.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct McpPayload {
    pub config_sig: String,
    pub servers: Vec<McpServerInfo>,
}

/// Marketplace payload: which source the listing belongs to.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MarketplacePayload {
    pub source: String,
    pub plugins: Vec<MarketplacePluginInfo>,
}

/// Git payload: the first page as the submenu shows it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GitPayload {
    pub status: GitStatus,
    pub commits: Vec<pacode_types::vcs::CommitRow>,
    pub has_more: bool,
}

/// How many commits the git file keeps (§8.2: "≈ latest 200").
pub const GIT_CACHE_COMMITS: usize = 200;

/// Everything read from disk at startup — `None` per entry on miss.
#[derive(Clone, Debug, Default)]
pub struct CacheSnapshot {
    pub models: Option<Entry<Vec<ModelInfo>>>,
    pub plugins: Option<Entry<Vec<PluginInfo>>>,
    pub marketplace: Option<Entry<Vec<MarketplacePluginInfo>>>,
    pub mcp_servers: Option<Entry<Vec<McpServerInfo>>>,
    pub sessions: Option<Entry<Vec<SessionMeta>>>,
    pub commands: Option<Entry<Vec<crate::commands::SlashCommand>>>,
    pub git: Option<Entry<GitPayload>>,
}

/// What the startup read did — snapshot plus the cap sweep's report.
#[derive(Clone, Debug, Default)]
pub struct StartupLoad {
    pub snapshot: CacheSnapshot,
    pub caps: CapsReport,
}

/// Read every cache file once and sweep the caps — the single blocking
/// operation §8.2 allows at startup (run it inside `spawn_blocking`, before
/// the first frame). Context validation lives here: a mismatched signature or
/// source reads as a miss and the file is dropped.
pub fn startup_load(cache: &DiskCache, ctx: &LoadCtx) -> StartupLoad {
    let s = ListKind::schema;

    let mcp_servers = cache
        .read::<McpPayload>("mcp", s(ListKind::McpServers))
        .and_then(|e| {
            if e.data.config_sig == *ctx.mcp_sig {
                Some(Entry {
                    written_at_ms: e.written_at_ms,
                    data: e.data.servers,
                })
            } else {
                cache.invalidate("mcp", s(ListKind::McpServers));
                None
            }
        });

    let marketplace = cache
        .read::<MarketplacePayload>("marketplace", s(ListKind::Marketplace))
        .and_then(|e| {
            if e.data.source == *ctx.marketplace_source {
                Some(Entry {
                    written_at_ms: e.written_at_ms,
                    data: e.data.plugins,
                })
            } else {
                cache.invalidate("marketplace", s(ListKind::Marketplace));
                None
            }
        });

    let snapshot = CacheSnapshot {
        models: cache.read("models", s(ListKind::Models)),
        plugins: cache.read("plugins", s(ListKind::Plugins)),
        marketplace,
        mcp_servers,
        sessions: cache.read("sessions", s(ListKind::Sessions)),
        commands: cache.read("commands", s(ListKind::Commands)),
        git: cache.read(&ListKind::GitLog.stem(&ctx.cwd), s(ListKind::GitLog)),
    };

    StartupLoad {
        snapshot,
        caps: cache.enforce_caps(),
    }
}

/// Fold the startup snapshot into state before the first frame. `lists`
/// records `Cached{stale}` per applied entry so pickers can badge
/// `offline copy`; sections without a hit stay `Empty` and show `loading…`
/// until their refresh lands.
pub fn apply_snapshot(state: &mut AppState, snap: CacheSnapshot, session_start_ms: u64) {
    let catalog_ttl = state.config.provider.catalog_ttl_secs;
    let now = pacode_types::time::now_ms();
    let mut applied = false;
    let mut mark = |kind: ListKind, written_at_ms: u64| {
        let stale = is_stale(kind, written_at_ms, now, session_start_ms, catalog_ttl);
        state.lists.apply_cached(kind, stale);
    };

    if let Some(e) = snap.commands {
        crate::commands::restore_dynamic_commands(e.data);
        applied = true;
    }
    if let Some(e) = snap.plugins {
        crate::commands::register_plugins(&e.data);
        state.plugins = e.data;
        mark(ListKind::Plugins, e.written_at_ms);
        applied = true;
    }
    if let Some(e) = snap.mcp_servers {
        crate::commands::register_mcp_servers(&e.data);
        state.mcp_servers = e.data;
        mark(ListKind::McpServers, e.written_at_ms);
        applied = true;
    }
    if let Some(e) = snap.marketplace {
        state.marketplace = e.data;
        mark(ListKind::Marketplace, e.written_at_ms);
        applied = true;
    }
    if let Some(e) = snap.sessions {
        state.sessions = e.data;
        mark(ListKind::Sessions, e.written_at_ms);
        applied = true;
    }
    if let Some(e) = snap.models {
        state.models = e.data;
        mark(ListKind::Models, e.written_at_ms);
        applied = true;
    }
    if let Some(e) = snap.git {
        state
            .git
            .apply_cached(e.data.commits, e.data.has_more, e.data.status);
        mark(ListKind::GitLog, e.written_at_ms);
        applied = true;
    }

    if applied {
        state.dirty = true;
    }
}

/// Write back the cache file(s) a fresh reply just refreshed. Runs after the
/// reply folded into state, so command registration is already reflected in
/// `commands` — which is why plugins/MCP replies also rewrite `Commands`.
/// All writes go to `spawn_blocking`; failures are logged inside `DiskCache`.
///
/// `GitLog` callers pass this only for first-page replies — a page append
/// would silently truncate the file to 200 mid-history rows.
pub fn write_after_reply(state: &AppState, kind: ListKind) {
    match kind {
        ListKind::Models => spawn_write(state, kind, state.models.clone()),
        ListKind::Plugins => {
            spawn_write(state, kind, state.plugins.clone());
            spawn_write(
                state,
                ListKind::Commands,
                crate::commands::dynamic_commands(),
            );
        }
        ListKind::Marketplace => {
            spawn_write(
                state,
                kind,
                MarketplacePayload {
                    source: state.config.plugins.marketplace.clone(),
                    plugins: state.marketplace.clone(),
                },
            );
        }
        ListKind::McpServers => {
            spawn_write(
                state,
                kind,
                McpPayload {
                    config_sig: mcp_sig(&state.config),
                    servers: state.mcp_servers.clone(),
                },
            );
            spawn_write(
                state,
                ListKind::Commands,
                crate::commands::dynamic_commands(),
            );
        }
        ListKind::Sessions => spawn_write(state, kind, state.sessions.clone()),
        ListKind::Commands => {
            // Never written directly — plugins/MCP replies carry it along.
        }
        ListKind::GitLog => {
            if let Some(status) = state.git.status {
                let payload = GitPayload {
                    status,
                    commits: state
                        .git
                        .commits
                        .iter()
                        .take(GIT_CACHE_COMMITS)
                        .cloned()
                        .collect(),
                    has_more: state.git.has_more,
                };
                spawn_write(state, kind, payload);
            }
        }
    }
}

/// Serialize + write one entry off the async thread.
fn spawn_write<T>(state: &AppState, kind: ListKind, payload: T)
where
    T: serde::Serialize + Send + 'static,
{
    let cache = state.cache.clone();
    let stem = kind.stem(&state.cwd()).into_owned();
    let schema = kind.schema();
    tokio::task::spawn_blocking(move || {
        cache.write(&stem, schema, &payload);
    });
}
