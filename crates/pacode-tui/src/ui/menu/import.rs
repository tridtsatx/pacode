//! The `Import` level (§5.7): one `Check` row per discovered MCP server or
//! skill, grouped under a `Section` per `ImportSource`. It is the only menu
//! where `enter` *does* something other than descend or edit — it applies the
//! marked set through `pacode_import::apply`, so the level declares
//! `enter_label` ("enter import" on the hint edge) instead of a row target.
//!
//! State split (§3.6): the filesystem scan — what `discover` found, each
//! entry's conflict status, the apply target — lives in `state.import` and is
//! rebuilt by [`on_open`] every time the level opens. The user's marks live
//! in `ui.checks`, saved with the level, so a `?`-Help detour cannot lose
//! them. `space` marks/unmarks the row under the cursor, `a` marks or clears
//! the cursor row's whole source group; the status line counts the marked
//! set. With nothing marked `enter` parks a "nothing selected" status and
//! stays put — no half-run import.
//!
//! Statuses: `New` rows start marked; `Same` ("already imported") and
//! `Conflict` ("⚠ already exists") start unmarked — an overwrite is always a
//! deliberate mark, never a default. The target applies with `force` so a
//! marked conflict row is honored (the legacy overlay could not express that
//! at all — it kept such rows out of the plan entirely).

use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_import::{
    ApplyTarget, Discovery, EntryStatus, ImportSource, ImportWarning, PlanEntry, PlanItem, apply,
    check_status, discover,
};
use pacode_types::ToastLevel;

use crate::binding::Binding;
use crate::keys::Action;
use crate::state::AppState;
use crate::ui::overlays::short_path;

use super::model::{Detail, DetailLine, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{EmptyState, MenuAction, MenuId, MenuUiState, MenuView, StatusLine};

#[cfg(test)]
#[path = "import_tests.rs"]
mod import_tests;

/// The declared `space` action's label — `on_action` matches on it.
const ACT_MARK: &str = "mark";
/// The declared `a` action's label.
const ACT_GROUP: &str = "group";

/// What `ImportSource::watch_paths` yielded for one source: the first
/// existing path — what the section tail shows — and the primary config path
/// that records where discovery looked. `found.is_none()` is the section's
/// "no config" mark (§5.7).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceScan {
    pub source: ImportSource,
    pub found: Option<PathBuf>,
    pub primary: PathBuf,
}

/// One importable entry — a discovered MCP server or skill — with the row id
/// the marks map keys on and the conflict status taken at scan time.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportEntry {
    pub id: RowId,
    pub entry: PlanEntry,
    pub status: EntryStatus,
}

/// The snapshot `state.import` holds (§5.7): the sources in
/// `ImportSource::all` order, the entries grouped under their source (MCP
/// rows first, then skills — the legacy overlay's order), the apply target
/// and the non-fatal scan warnings.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportScan {
    pub target: ApplyTarget,
    pub sources: Vec<SourceScan>,
    pub entries: Vec<ImportEntry>,
    pub warnings: Vec<ImportWarning>,
}

impl ImportScan {
    /// Group a discovery by source and stamp every entry's status. `force` in
    /// `target` is what makes a marked `⚠` row a real overwrite (§5.7).
    pub(crate) fn from_discovery(
        discovery: Discovery,
        target: ApplyTarget,
        home: &Path,
        cwd: &Path,
    ) -> Self {
        let sources = ImportSource::all()
            .iter()
            .map(|&source| {
                let watched = source.watch_paths(home, cwd);
                let found = watched.iter().find(|p| p.exists()).cloned();
                SourceScan {
                    source,
                    found,
                    primary: watched.into_iter().next().unwrap_or_default(),
                }
            })
            .collect();

        let mut entries = Vec::new();
        for &source in ImportSource::all() {
            for m in discovery.mcp.iter().filter(|m| m.source == source) {
                let entry = PlanEntry::mcp(m.clone());
                let status = check_status(&entry, &target);
                entries.push(ImportEntry {
                    id: entry_id(&entry),
                    entry,
                    status,
                });
            }
            for s in discovery.skills.iter().filter(|s| s.source == source) {
                let entry = PlanEntry::skill(s.clone());
                let status = check_status(&entry, &target);
                entries.push(ImportEntry {
                    id: entry_id(&entry),
                    entry,
                    status,
                });
            }
        }

        Self {
            target,
            sources,
            entries,
            warnings: discovery.errors,
        }
    }

    /// The entries of one source group — `a` toggles these together.
    fn group(&self, source: ImportSource) -> impl Iterator<Item = &ImportEntry> {
        self.entries
            .iter()
            .filter(move |e| e.entry.source() == source)
    }
}

/// The menu view — rebuilt per frame from `state.import`; the marks come
/// from `ui.checks` so the rebuild cannot lose them.
pub fn view(state: &AppState, ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Import);
    view.enter_label = Some("import");
    view.empty = EmptyState::new(
        "nothing to import",
        "no MCP servers or skills in the supported agent configs",
    );
    let Some(scan) = &state.import else {
        return view.with_actions(actions());
    };

    let mut rows: Vec<MenuRow> = Vec::with_capacity(scan.entries.len() + scan.sources.len());
    for src in &scan.sources {
        let right = src
            .found
            .as_deref()
            .map(short_path)
            .unwrap_or_else(|| "no config".to_string());
        rows.push(MenuRow::Section {
            label: format!("[{}]", src.source.label()),
            right: Some(right),
        });
        for e in scan.group(src.source) {
            rows.push(entry_row(state, scan, e, ui));
        }
    }
    view.rows = rows;
    // A parked status (the "nothing selected" note) wins for its one frame;
    // otherwise the status line is the marked-set counter (§5.7).
    view.status = ui.status.clone().or_else(|| selection_status(scan, ui));
    view.with_actions(actions())
}

/// The level's open hook: the filesystem scan `state.import` holds (§5.7).
/// Read-only — `discover` only ever reads the watched paths; the write side
/// is `enter` alone.
pub fn on_open(state: &mut AppState) -> Vec<Action> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cwd = state.cwd();
    state.import = Some(scan(&home, &cwd, &state.paths));
    vec![]
}

/// `enter` on a `Check` row is the level's apply (§5.7): the marked set — not
/// the row under the cursor — is imported. No marks parks a status and keeps
/// the menu open; a non-empty plan applies the way the legacy overlay did
/// (toast, config reload, list-cache invalidation) and closes the menu.
pub fn on_row(
    state: &mut AppState,
    stack: &mut MenuStack,
    ui: &mut MenuUiState,
    _row: &RowId,
) -> Vec<Action> {
    let Some(scan) = &state.import else {
        return vec![];
    };
    let plan: Vec<PlanEntry> = scan
        .entries
        .iter()
        .filter(|e| checked(e, ui))
        .map(|e| e.entry.clone())
        .collect();
    if plan.is_empty() {
        ui.status = Some(StatusLine::info(
            "nothing selected — space marks an entry, a marks the group",
        ));
        return vec![];
    }
    let report = apply(&plan, &scan.target);
    let s = report.imported_servers;
    let sk = report.imported_skills;
    if report.errors.is_empty() {
        state.push_toast(
            ToastLevel::Success,
            format!("imported {s} servers, {sk} skills"),
            None,
            Instant::now(),
        );
    } else {
        let failed = report.errors.len();
        state.push_toast(
            ToastLevel::Error,
            format!("imported {s} servers, {sk} skills · {failed} failed"),
            Some(report.errors.join("\n")),
            Instant::now(),
        );
    }
    if let Ok(new_cfg) = pacode_config::load(&state.paths) {
        state.config = new_cfg;
    }
    // §8.2: the import rewrote `[mcp.servers]` — the cached server list and
    // the prompt-command registry it feeds are now wrong; drop both (the
    // next `ListMcpServers` reply rewrites them).
    state.invalidate_cache(crate::cache::ListKind::McpServers);
    state.invalidate_cache(crate::cache::ListKind::Commands);
    state.import = None;
    while stack.pop().is_some() {}
    vec![]
}

/// Declared actions (§3.4): `space` flips the row under the cursor, `a`
/// marks the cursor row's source group — or clears it when every entry is
/// marked already. Either retires a parked status.
pub fn on_action(
    state: &mut AppState,
    ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    let (Some(row), Some(scan)) = (row, state.import.as_ref()) else {
        return vec![];
    };
    match action.label {
        ACT_MARK => {
            let Some(e) = scan.entries.iter().find(|e| e.id == *row) else {
                return vec![];
            };
            let next = !checked(e, ui);
            ui.checks.insert(e.id.clone(), next);
        }
        ACT_GROUP => {
            let Some(source) = scan
                .entries
                .iter()
                .find(|e| e.id == *row)
                .map(|e| e.entry.source())
            else {
                return vec![];
            };
            let any_unmarked = scan.group(source).any(|e| !checked(e, ui));
            let ids: Vec<RowId> = scan.group(source).map(|e| e.id.clone()).collect();
            for id in ids {
                ui.checks.insert(id, any_unmarked);
            }
        }
        _ => return vec![],
    }
    ui.status = None;
    vec![]
}

/// The scan behind the level: `discover` the sources, then build the apply
/// target the legacy overlay used — `config.toml`'s sibling `skills/` dir.
fn scan(home: &Path, cwd: &Path, paths: &pacode_config::Paths) -> ImportScan {
    let discovery = discover(home, cwd, ImportSource::all());
    let config_dir = paths.config_file.parent().unwrap_or(Path::new("."));
    // `force`: a `⚠` row the user marked is a deliberate overwrite (§5.7) —
    // `apply` has to honor it, not skip it the way the legacy overlay did.
    let target = ApplyTarget::new(&paths.config_file, config_dir.join("skills"), true);
    ImportScan::from_discovery(discovery, target, home, cwd)
}

/// The mark a row carries: the `ui.checks` override wins; else the status
/// default — only `New` entries start marked (§5.7: a `⚠` row is a
/// deliberate mark, never a silent default).
fn checked(entry: &ImportEntry, ui: &MenuUiState) -> bool {
    ui.checks
        .get(&entry.id)
        .copied()
        .unwrap_or(matches!(entry.status, EntryStatus::New))
}

/// A row's id — `{source}:{kind}:{name}`; the discovery dedups by name per
/// kind, so the triple is unique.
fn entry_id(entry: &PlanEntry) -> RowId {
    RowId::new(format!(
        "{}:{}:{}",
        entry.source().id(),
        entry.kind_str(),
        entry.name()
    ))
}

/// One discovered entry as a `Check` row (§5.7): `[mcp] name`, the `⚠` tail
/// for a Same/Conflict, the detail card explaining what gets imported or
/// overwritten.
fn entry_row(state: &AppState, scan: &ImportScan, e: &ImportEntry, ui: &MenuUiState) -> MenuRow {
    let warn = match e.status {
        EntryStatus::New => None,
        EntryStatus::Same => Some("already imported".to_string()),
        EntryStatus::Conflict => Some("already exists".to_string()),
    };
    MenuRow::Check {
        id: e.id.clone(),
        label: format!("[{}] {}", e.entry.kind_str(), e.entry.name()),
        checked: checked(e, ui),
        warn,
        detail: entry_detail(state, scan, e),
    }
}

/// The detail card (§5.7): what the entry is, where it was found, what it
/// will look like in pacode — and for a conflict, what it replaces.
fn entry_detail(state: &AppState, scan: &ImportScan, e: &ImportEntry) -> Detail {
    let mut lines = vec![
        field("kind", kind_word(e.entry.kind_str())),
        field("source", e.entry.source().label()),
    ];
    match &e.entry.item {
        PlanItem::Mcp(m) => {
            lines.push(field("from", short_path(&m.origin)));
            let value = e.entry.detail();
            if !value.is_empty() {
                let key = if m.server.url.is_some() {
                    "url"
                } else {
                    "command"
                };
                lines.push(field(key, value));
            }
            lines.push(field(
                "target",
                format!("[mcp.servers] in {}", short_path(&scan.target.config_file)),
            ));
        }
        PlanItem::Skill(s) => {
            lines.push(field("from", short_path(&s.dir)));
            if !s.description.is_empty() {
                lines.push(field("description", s.description.clone()));
            }
            lines.push(field(
                "target",
                short_path(&scan.target.skills_dir.join(&s.name)),
            ));
        }
    }
    match e.status {
        EntryStatus::New => {
            lines.push(DetailLine::Text("new — not in pacode yet".to_string()));
        }
        EntryStatus::Same => {
            lines.push(DetailLine::Text(
                "identical entry already configured — importing rewrites it unchanged".to_string(),
            ));
        }
        EntryStatus::Conflict => {
            if let PlanItem::Mcp(m) = &e.entry.item
                && let Some(current) = state.config.mcp.servers.get(&m.name)
            {
                lines.push(field("current", mcp_summary(current)));
            }
            lines.push(DetailLine::Text(
                "differs from the configured entry — import overwrites it".to_string(),
            ));
        }
    }
    Detail {
        title: e.entry.name().to_string(),
        lines,
        ..Detail::default()
    }
}

/// `npx foo --bar` or the URL — how the configured server reads in one line.
fn mcp_summary(server: &pacode_import::McpServerConfig) -> String {
    if let Some(url) = &server.url {
        url.clone()
    } else if server.args.is_empty() {
        server.command.clone()
    } else {
        format!("{} {}", server.command, server.args.join(" "))
    }
}

fn kind_word(kind: &str) -> &'static str {
    match kind {
        "mcp" => "MCP server",
        _ => "skill",
    }
}

/// `marked of total selected` — plus the unreadable-source count when the
/// scan hit files it could not read (§5.7 keeps the failures visible).
fn selection_status(scan: &ImportScan, ui: &MenuUiState) -> Option<StatusLine> {
    let mut text = if scan.entries.is_empty() {
        "nothing importable found".to_string()
    } else {
        let marked = scan.entries.iter().filter(|e| checked(e, ui)).count();
        let total = scan.entries.len();
        format!("{marked} of {total} selected")
    };
    if !scan.warnings.is_empty() {
        let n = scan.warnings.len();
        let noun = if n == 1 { "file" } else { "files" };
        text.push_str(&format!(" · {n} {noun} unreadable"));
    }
    Some(StatusLine::info(text))
}

fn actions() -> Vec<MenuAction> {
    vec![
        MenuAction::new(
            Binding {
                code: KeyCode::Char(' '),
                mods: KeyModifiers::NONE,
            },
            ACT_MARK,
        ),
        MenuAction::new(
            Binding {
                code: KeyCode::Char('a'),
                mods: KeyModifiers::NONE,
            },
            ACT_GROUP,
        ),
    ]
}

fn field(label: &str, value: impl Into<String>) -> DetailLine {
    DetailLine::Field {
        label: label.to_string(),
        value: value.into(),
    }
}
