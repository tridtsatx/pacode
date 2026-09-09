//! Applying discovered MCP servers and skills to pacode configuration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pacode_types::Config;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::{DiscoveredMcp, DiscoveredSkill, ImportSource};

#[cfg(test)]
#[path = "apply_tests.rs"]
mod apply_tests;

/// A single discovered item to be imported.
#[derive(Clone, Debug, PartialEq)]
pub enum PlanItem {
    Mcp(DiscoveredMcp),
    Skill(DiscoveredSkill),
}

/// A planned import entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanEntry {
    pub item: PlanItem,
}

impl PlanEntry {
    pub fn mcp(discovered: DiscoveredMcp) -> Self {
        Self {
            item: PlanItem::Mcp(discovered),
        }
    }

    pub fn skill(discovered: DiscoveredSkill) -> Self {
        Self {
            item: PlanItem::Skill(discovered),
        }
    }

    pub fn name(&self) -> &str {
        match &self.item {
            PlanItem::Mcp(m) => &m.name,
            PlanItem::Skill(s) => &s.name,
        }
    }

    pub fn source(&self) -> ImportSource {
        match &self.item {
            PlanItem::Mcp(m) => m.source,
            PlanItem::Skill(s) => s.source,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match &self.item {
            PlanItem::Mcp(_) => "mcp",
            PlanItem::Skill(_) => "skill",
        }
    }

    pub fn detail(&self) -> String {
        match &self.item {
            PlanItem::Mcp(m) => {
                if let Some(ref url) = m.server.url {
                    url.clone()
                } else if m.server.args.is_empty() {
                    m.server.command.clone()
                } else {
                    let cmd = &m.server.command;
                    let args = m.server.args.join(" ");
                    format!("{cmd} {args}")
                }
            }
            PlanItem::Skill(s) => s.description.clone(),
        }
    }
}

/// Destination paths and flags for applying imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyTarget {
    pub config_file: PathBuf,
    pub skills_dir: PathBuf,
    pub force: bool,
}

impl ApplyTarget {
    pub fn new(
        config_file: impl Into<PathBuf>,
        skills_dir: impl Into<PathBuf>,
        force: bool,
    ) -> Self {
        Self {
            config_file: config_file.into(),
            skills_dir: skills_dir.into(),
            force,
        }
    }
}

/// Status of an entry relative to current pacode configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryStatus {
    New,
    Same,
    Conflict,
}

impl EntryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Same => "same",
            Self::Conflict => "conflict",
        }
    }
}

/// Result outcome for a single applied entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Imported,
    SkippedSame,
    SkippedConflict,
    Failed(String),
}

/// Outcome information for an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedEntry {
    pub name: String,
    pub kind: &'static str,
    pub source: ImportSource,
    pub outcome: ApplyOutcome,
}

/// Summary report of an apply operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub entries: Vec<AppliedEntry>,
    pub imported_servers: usize,
    pub imported_skills: usize,
    pub skipped_servers: usize,
    pub skipped_skills: usize,
    pub errors: Vec<String>,
}

/// Check the status of a plan entry against target pacode configuration.
pub fn check_status(entry: &PlanEntry, target: &ApplyTarget) -> EntryStatus {
    match &entry.item {
        PlanItem::Mcp(discovered) => {
            let existing_cfg = load_existing_config(&target.config_file);
            match existing_cfg {
                Some(cfg) => {
                    if let Some(existing) = cfg.mcp.servers.get(&discovered.name) {
                        if existing == &discovered.server {
                            EntryStatus::Same
                        } else {
                            EntryStatus::Conflict
                        }
                    } else {
                        EntryStatus::New
                    }
                }
                None => EntryStatus::New,
            }
        }
        PlanItem::Skill(discovered) => {
            let dest_dir = target.skills_dir.join(&discovered.name);
            if !dest_dir.exists() {
                EntryStatus::New
            } else if dirs_identical(&discovered.dir, &dest_dir) {
                EntryStatus::Same
            } else {
                EntryStatus::Conflict
            }
        }
    }
}

/// Apply a list of plan entries to the specified target.
pub fn apply(plan: &[PlanEntry], target: &ApplyTarget) -> ApplyReport {
    let mut report = ApplyReport::default();
    let mut pending_mcp: Vec<&DiscoveredMcp> = Vec::new();

    for entry in plan {
        let status = check_status(entry, target);
        match &entry.item {
            PlanItem::Mcp(mcp) => match status {
                EntryStatus::Same | EntryStatus::Conflict if !target.force => {
                    report.skipped_servers += 1;
                    let outcome = if status == EntryStatus::Same {
                        ApplyOutcome::SkippedSame
                    } else {
                        ApplyOutcome::SkippedConflict
                    };
                    report.entries.push(AppliedEntry {
                        name: mcp.name.clone(),
                        kind: "mcp",
                        source: mcp.source,
                        outcome,
                    });
                }
                _ => {
                    pending_mcp.push(mcp);
                    report.imported_servers += 1;
                    report.entries.push(AppliedEntry {
                        name: mcp.name.clone(),
                        kind: "mcp",
                        source: mcp.source,
                        outcome: ApplyOutcome::Imported,
                    });
                }
            },
            PlanItem::Skill(skill) => match status {
                EntryStatus::Same | EntryStatus::Conflict if !target.force => {
                    report.skipped_skills += 1;
                    let outcome = if status == EntryStatus::Same {
                        ApplyOutcome::SkippedSame
                    } else {
                        ApplyOutcome::SkippedConflict
                    };
                    report.entries.push(AppliedEntry {
                        name: skill.name.clone(),
                        kind: "skill",
                        source: skill.source,
                        outcome,
                    });
                }
                _ => match apply_skill(skill, target) {
                    Ok(()) => {
                        report.imported_skills += 1;
                        report.entries.push(AppliedEntry {
                            name: skill.name.clone(),
                            kind: "skill",
                            source: skill.source,
                            outcome: ApplyOutcome::Imported,
                        });
                    }
                    Err(e) => {
                        report.errors.push(e.clone());
                        report.entries.push(AppliedEntry {
                            name: skill.name.clone(),
                            kind: "skill",
                            source: skill.source,
                            outcome: ApplyOutcome::Failed(e),
                        });
                    }
                },
            },
        }
    }

    if !pending_mcp.is_empty()
        && let Err(e) = write_mcp_servers(&pending_mcp, &target.config_file)
    {
        report.errors.push(e.clone());
        for entry in &mut report.entries {
            if entry.kind == "mcp" && entry.outcome == ApplyOutcome::Imported {
                entry.outcome = ApplyOutcome::Failed(e.clone());
            }
        }
        report.imported_servers = 0;
    }

    report
}

fn apply_skill(skill: &DiscoveredSkill, target: &ApplyTarget) -> Result<(), String> {
    if !skill.dir.exists() {
        let dir = skill.dir.display();
        return Err(format!("source skill directory {dir} does not exist"));
    }
    let dest_dir = target.skills_dir.join(&skill.name);
    if dest_dir.exists() {
        let dest = dest_dir.display();
        std::fs::remove_dir_all(&dest_dir)
            .map_err(|e| format!("failed to remove existing {dest}: {e}"))?;
    }
    let dest = dest_dir.display();
    copy_dir_all(&skill.dir, &dest_dir)
        .map_err(|e| format!("failed to copy skill to {dest}: {e}"))?;
    Ok(())
}

fn write_mcp_servers(servers: &[&DiscoveredMcp], config_file: &Path) -> Result<(), String> {
    let cfg = config_file.display();
    let mut doc = if config_file.exists() {
        let content = std::fs::read_to_string(config_file)
            .map_err(|e| format!("failed to read {cfg}: {e}"))?;
        content
            .parse::<DocumentMut>()
            .map_err(|e| format!("failed to parse {cfg}: {e}"))?
    } else {
        DocumentMut::new()
    };

    if !doc.contains_key("mcp") {
        let mut t = Table::new();
        t.set_implicit(true);
        doc["mcp"] = Item::Table(t);
    }
    let mcp = doc["mcp"]
        .as_table_mut()
        .ok_or_else(|| "[mcp] in config is not a table".to_string())?;

    if !mcp.contains_key("servers") {
        let mut t = Table::new();
        t.set_implicit(true);
        mcp["servers"] = Item::Table(t);
    }
    let srv_table = mcp["servers"]
        .as_table_mut()
        .ok_or_else(|| "[mcp.servers] in config is not a table".to_string())?;

    for s in servers {
        let name = &s.name;
        if !srv_table.contains_key(name) {
            srv_table.insert(name, Item::Table(Table::new()));
        }
        let srv = srv_table[name]
            .as_table_mut()
            .ok_or_else(|| format!("[mcp.servers.{name}] is not a table"))?;

        if let Some(ref url_str) = s.server.url {
            srv.remove("command");
            srv.remove("args");
            srv.remove("env");
            srv.insert("url", value(url_str.clone()));
            if !s.server.headers.is_empty() {
                let mut hdr_tbl = InlineTable::new();
                for (k, v) in &s.server.headers {
                    hdr_tbl.insert(k, Value::from(v.as_str()));
                }
                srv.insert("headers", Item::Value(Value::InlineTable(hdr_tbl)));
            } else {
                srv.remove("headers");
            }
        } else {
            srv.remove("url");
            srv.remove("headers");
            srv.insert("command", value(s.server.command.clone()));
            let mut arr = Array::new();
            for a in &s.server.args {
                arr.push(a.as_str());
            }
            srv.insert("args", Item::Value(Value::Array(arr)));
            if !s.server.env.is_empty() {
                let mut env_tbl = InlineTable::new();
                for (k, v) in &s.server.env {
                    env_tbl.insert(k, Value::from(v.as_str()));
                }
                srv.insert("env", Item::Value(Value::InlineTable(env_tbl)));
            } else {
                srv.remove("env");
            }
        }

        srv.insert("enabled", value(s.server.enabled));
        srv.insert("lazy", value(s.server.lazy));
        if s.server.timeout_secs != 60 {
            srv.insert("timeout_secs", value(s.server.timeout_secs as i64));
        }
    }

    if let Some(parent) = config_file.parent() {
        let p = parent.display();
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {p}: {e}"))?;
    }
    std::fs::write(config_file, doc.to_string())
        .map_err(|e| format!("failed to write {cfg}: {e}"))?;

    Ok(())
}

fn load_existing_config(path: &Path) -> Option<Config> {
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str::<Config>(&content).ok()
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn dirs_identical(dir_a: &Path, dir_b: &Path) -> bool {
    let mut files_a = BTreeMap::new();
    let mut files_b = BTreeMap::new();
    if collect_files(dir_a, dir_a, &mut files_a).is_err() {
        return false;
    }
    if collect_files(dir_b, dir_b, &mut files_b).is_err() {
        return false;
    }
    files_a == files_b
}

fn collect_files(
    root: &Path,
    current: &Path,
    out: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(e) => return Err(std::io::Error::other(e)),
        };
        if entry.file_type()?.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let bytes = std::fs::read(&path)?;
            out.insert(rel, bytes);
        }
    }
    Ok(())
}
