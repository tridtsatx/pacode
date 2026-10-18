//! Memory tools over `pacode-memory`: `memory_write`, `memory_recall`, `memory_forget`,
//! `memory_list`, plus `memory_read` kept as an alias of `memory_recall`.
//!
//! Two scopes: `global` (`<config dir>/memory.db`) and `project` (`<cwd>/.pacode/memory.db`;
//! consider adding `.pacode/memory.db*` to `.gitignore`, pacode never edits it).
//!
//! Store calls are tiny SQLite queries but synchronous, so each tool runs them on
//! `tokio::task::spawn_blocking` (the `rt` feature is already enabled for this crate).
//! Subagents share one `MemoryStore` per (config dir, cwd) through [`store_for_dirs`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use pacode_memory::{
    Entry, Kind, MemoryError, MemoryStore, NewEntry, RecallRequest, Scope, WriteOutcome,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::helpers::{cap_output, parse_input};
use crate::{Tool, ToolCtx, ToolError, ToolKind, ToolOutput};

pub const WRITE_NAME: &str = "memory_write";
pub const READ_NAME: &str = "memory_read";
pub const RECALL_NAME: &str = "memory_recall";
pub const FORGET_NAME: &str = "memory_forget";
pub const LIST_NAME: &str = "memory_list";

/// Characters of an entry's text shown per recall hit.
const HIT_TEXT_CHARS: usize = 600;
const DEFAULT_LIST_LIMIT: usize = 50;

/// Directory of pacode's global config/data (`~/.config/pacode` by default).
/// Respects `PACODE_HOME`, `PACODE_CONFIG`, and `XDG_CONFIG_HOME`.
pub fn global_memory_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("PACODE_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(home);
    }
    if let Some(cfg) = std::env::var_os("PACODE_CONFIG").filter(|s| !s.is_empty()) {
        let p = PathBuf::from(cfg);
        if let Some(parent) = p.parent() {
            return parent.to_path_buf();
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(xdg).join("pacode");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(home).join(".config").join("pacode");
    }
    PathBuf::from(".pacode")
}

/// Legacy flat file of the global scope (imported once, then renamed `.migrated`).
pub fn global_memory_path() -> PathBuf {
    global_memory_dir().join("memory.md")
}

/// Legacy flat file of the project scope.
pub fn project_memory_path(cwd: &Path) -> PathBuf {
    cwd.join(".pacode").join("memory.md")
}

type StoreKey = (PathBuf, PathBuf);

/// Cache of open stores. Owner: this module. Cap: [`STORE_CACHE_CAP`] entries (one per
/// distinct config dir + cwd, normally 1-2 per process); on overflow the whole map is
/// cleared (stores close when their last `Arc` drops). No timers, nothing ticks.
static STORES: OnceLock<Mutex<HashMap<StoreKey, Arc<MemoryStore>>>> = OnceLock::new();
const STORE_CACHE_CAP: usize = 16;

pub fn store_for_dirs(global_dir: &Path, cwd: &Path) -> Arc<MemoryStore> {
    let map = STORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|p| p.into_inner());
    let key = (global_dir.to_path_buf(), cwd.to_path_buf());
    if let Some(s) = map.get(&key) {
        return s.clone();
    }
    if map.len() >= STORE_CACHE_CAP {
        map.clear();
    }
    let store = Arc::new(MemoryStore::for_dirs(global_dir, cwd));
    map.insert(key, store.clone());
    store
}

fn store_for(cwd: &Path) -> Arc<MemoryStore> {
    store_for_dirs(&global_memory_dir(), cwd)
}

fn map_err(e: MemoryError) -> ToolError {
    match e {
        MemoryError::Invalid(m) => ToolError::InvalidInput(m),
        other => ToolError::Failed(other.to_string()),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ToolError::failed(format!("memory task failed: {e}")))?
}

fn parse_scopes(scope: Option<&str>) -> Result<Vec<Scope>, ToolError> {
    match scope.map(str::trim) {
        None | Some("") | Some("both") => Ok(Scope::ALL.to_vec()),
        Some(s) => Ok(vec![Scope::parse(s).map_err(map_err)?]),
    }
}

fn parse_kind(kind: Option<&str>) -> Result<Option<Kind>, ToolError> {
    match kind.map(str::trim) {
        None | Some("") => Ok(None),
        Some(k) => Kind::parse(k).map(Some).map_err(map_err),
    }
}

fn format_age(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        86_400..=5_183_999 => format!("{}d ago", secs / 86_400),
        _ => format!("{}mo ago", secs / 2_592_000),
    }
}

fn now_ms() -> i64 {
    pacode_types::time::now_ms() as i64
}

// ---------------------------------------------------------------- memory_write

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct WriteInput {
    scope: String,
    kind: Option<String>,
    title: Option<String>,
    text: String,
    tags: Vec<String>,
    pinned: bool,
    supersedes: Option<String>,
}

pub(crate) fn run_write(
    store: &MemoryStore,
    args: WriteInput,
    source: Option<String>,
) -> Result<String, ToolError> {
    let scope = Scope::parse(&args.scope).map_err(map_err)?;
    let kind = parse_kind(args.kind.as_deref())?.unwrap_or(Kind::Fact);
    let mut entry = NewEntry::new(kind, args.title.unwrap_or_default(), args.text);
    entry.tags = args.tags;
    entry.pinned = args.pinned;
    entry.supersedes = args.supersedes;
    entry.source = source;
    let WriteOutcome {
        id,
        created,
        superseded,
        ..
    } = store.write(scope, entry).map_err(map_err)?;
    let verb = if created { "stored" } else { "updated" };
    let mut out = format!("{verb} {id} ({})", scope.as_str());
    if superseded {
        out.push_str(" (superseded the old entry)");
    }
    Ok(out)
}

pub struct MemoryWriteTool;

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        WRITE_NAME
    }

    fn description(&self) -> &str {
        "Save a durable memory (a fact, user preference, decision, lesson or gotcha) that \
         should survive across sessions. scope 'global' is shared by all projects, 'project' \
         is only this repo. Give a short title (<=80 chars) and the text (<=2000 chars). \
         A near-duplicate of an existing entry updates it in place ('updated <id>') instead \
         of adding a copy. Use 'supersedes' with an old id to replace a stale entry, and \
         pinned=true only for the few things every session must see."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["scope", "text"],
            "properties": {
                "scope": {"type": "string", "enum": ["global", "project"],
                    "description": "'global' (all projects) or 'project' (this repo only)."},
                "kind": {"type": "string",
                    "enum": ["fact", "preference", "decision", "lesson", "gotcha"],
                    "description": "Default 'fact'."},
                "title": {"type": "string", "description": "Short label, <= 80 chars. Derived from the text when omitted."},
                "text": {"type": "string", "description": "The memory itself, <= 2000 chars."},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "Up to 8 short tags."},
                "pinned": {"type": "boolean", "description": "Show in full in every session's prompt. Use sparingly."},
                "supersedes": {"type": "string", "description": "Id of an older entry this replaces."}
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _) = parse_input::<WriteInput>(input)?;
        let scope = Scope::parse(&args.scope).map_err(map_err)?;
        // Project scope writes inside the repository: ask like any other project write.
        // Global scope is pacode-owned configuration and needs no prompt.
        if scope == Scope::Project {
            let title = args.title.as_deref().unwrap_or(args.text.as_str());
            let detail = format!(
                "memory_write: {}",
                pacode_types::truncate_head_tail(title, 200)
            );
            ctx.require_permission("Write .pacode/memory.db", detail, None)
                .await?;
        }
        let store = store_for(&ctx.cwd);
        let source = Some(ctx.agent_name.clone()).filter(|s| !s.is_empty());
        let text = blocking(move || run_write(&store, args, source)).await?;
        Ok(ToolOutput::text(text.clone())
            .with_title(format!("{WRITE_NAME} ({})", scope.as_str()))
            .with_preview(text))
    }
}

// --------------------------------------------------------------- memory_recall

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct RecallInput {
    query: String,
    scope: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
}

fn format_hit(e: &Entry, now: i64) -> String {
    let mut head = format!(
        "[{}/{}] {} {}",
        e.scope.as_str(),
        e.kind.as_str(),
        e.id,
        e.title
    );
    head.push_str(&format!(" ({}", format_age(now - e.updated_ms)));
    if e.recall_count > 0 {
        head.push_str(&format!(", recalled {}x", e.recall_count));
    }
    if e.pinned {
        head.push_str(", pinned");
    }
    head.push(')');
    if !e.tags.is_empty() {
        head.push_str(&format!(" #{}", e.tags.join(" #")));
    }
    let text: String = e.text.chars().take(HIT_TEXT_CHARS).collect();
    let ellipsis = if e.text.chars().count() > HIT_TEXT_CHARS {
        "…"
    } else {
        ""
    };
    format!("{head}\n  {}{ellipsis}", text.replace('\n', "\n  "))
}

pub(crate) fn run_recall(store: &MemoryStore, args: RecallInput) -> Result<String, ToolError> {
    let mut req = RecallRequest::new(args.query);
    req.scopes = parse_scopes(args.scope.as_deref())?;
    req.kind = parse_kind(args.kind.as_deref())?;
    if let Some(l) = args.limit {
        req.limit = l;
    }
    let hits = store.recall(&req).map_err(map_err)?;
    if hits.is_empty() {
        return Ok("no memories found".to_string());
    }
    let now = now_ms();
    let body: Vec<String> = hits.iter().map(|h| format_hit(&h.entry, now)).collect();
    Ok(format!("{} memories\n{}", hits.len(), body.join("\n")))
}

const RECALL_SCHEMA_DESC: &str = "Search saved memories (BM25 full-text, boosted by recency, \
    pinned and past use). Empty query lists pinned + most recent entries. Searches both \
    scopes unless 'scope' is given. Returns id, kind, title and text for up to 'limit' (default 5, max 20).";

fn recall_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Words to search for. Empty lists pinned + recent."},
            "scope": {"type": "string", "enum": ["global", "project", "both"], "description": "Default 'both'."},
            "kind": {"type": "string", "enum": ["fact", "preference", "decision", "lesson", "gotcha"]},
            "limit": {"type": "integer", "minimum": 1, "maximum": 20, "description": "Default 5."}
        }
    })
}

async fn call_recall(name: &str, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
    let (args, accept_large) = parse_input::<RecallInput>(input)?;
    let store = store_for(&ctx.cwd);
    let raw = blocking(move || run_recall(&store, args)).await?;
    let content = cap_output(&raw, accept_large, ctx.output_cap_chars);
    let preview = raw.lines().next().unwrap_or_default().to_string();
    Ok(ToolOutput::text(content)
        .with_title(name)
        .with_preview(preview))
}

pub struct MemoryRecallTool;

#[async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str {
        RECALL_NAME
    }
    fn description(&self) -> &str {
        RECALL_SCHEMA_DESC
    }
    fn schema(&self) -> Value {
        recall_schema()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        call_recall(RECALL_NAME, input, ctx).await
    }
}

/// Backward-compatible alias of [`MemoryRecallTool`] (old name `memory_read`).
pub struct MemoryReadTool;

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        READ_NAME
    }
    fn description(&self) -> &str {
        "Alias of memory_recall: search saved memories, or list pinned + recent ones when \
         no query is given. Optional scope 'global' | 'project' | 'both'."
    }
    fn schema(&self) -> Value {
        recall_schema()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        call_recall(READ_NAME, input, ctx).await
    }
}

// --------------------------------------------------------- memory_forget / list

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ForgetInput {
    id: String,
}

pub(crate) fn run_forget(store: &MemoryStore, args: ForgetInput) -> Result<String, ToolError> {
    let id = args.id.trim();
    if id.is_empty() {
        return Err(ToolError::invalid("id is required"));
    }
    match store.forget(id) {
        Ok(e) => Ok(format!(
            "forgot {} \"{}\" ({})",
            e.id,
            e.title,
            e.scope.as_str()
        )),
        Err(MemoryError::NotFound(_)) => {
            Err(ToolError::failed(format!("no memory with id '{id}'")))
        }
        Err(e) => Err(map_err(e)),
    }
}

pub struct MemoryForgetTool;

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        FORGET_NAME
    }
    fn description(&self) -> &str {
        "Forget a memory by id (from memory_recall / memory_list / the prompt index). Use it \
         for wrong or obsolete entries. It is hidden immediately and erased for good after 90 days."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {"id": {"type": "string", "description": "Memory id."}}
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, _) = parse_input::<ForgetInput>(input)?;
        let store = store_for(&ctx.cwd);
        let text = blocking(move || run_forget(&store, args)).await?;
        Ok(ToolOutput::text(text.clone())
            .with_title(FORGET_NAME)
            .with_preview(text))
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct ListInput {
    scope: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
}

pub(crate) fn run_list(store: &MemoryStore, args: ListInput) -> Result<String, ToolError> {
    let scopes = parse_scopes(args.scope.as_deref())?;
    let kind = parse_kind(args.kind.as_deref())?;
    let limit = args.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    let entries = store.list(&scopes, kind, limit).map_err(map_err)?;
    if entries.is_empty() {
        return Ok("no memories".to_string());
    }
    let now = now_ms();
    let lines: Vec<String> = entries
        .iter()
        .map(|e| {
            format!(
                "[{}/{}] {} {} ({}{})",
                e.scope.as_str(),
                e.kind.as_str(),
                e.id,
                e.title,
                format_age(now - e.updated_ms),
                if e.pinned { ", pinned" } else { "" }
            )
        })
        .collect();
    Ok(format!("{} memories\n{}", entries.len(), lines.join("\n")))
}

pub struct MemoryListTool;

#[async_trait]
impl Tool for MemoryListTool {
    fn name(&self) -> &str {
        LIST_NAME
    }
    fn description(&self) -> &str {
        "List memory titles (pinned first, then most recently updated) with ids; no text. \
         Use memory_recall to read the content."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string", "enum": ["global", "project", "both"]},
                "kind": {"type": "string", "enum": ["fact", "preference", "decision", "lesson", "gotcha"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Default 50."}
            }
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn call(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let (args, accept_large) = parse_input::<ListInput>(input)?;
        let store = store_for(&ctx.cwd);
        let raw = blocking(move || run_list(&store, args)).await?;
        let content = cap_output(&raw, accept_large, ctx.output_cap_chars);
        let preview = raw.lines().next().unwrap_or_default().to_string();
        Ok(ToolOutput::text(content)
            .with_title(LIST_NAME)
            .with_preview(preview))
    }
}

#[cfg(test)]
#[path = "memory_unit_tests.rs"]
mod memory_unit_tests;
