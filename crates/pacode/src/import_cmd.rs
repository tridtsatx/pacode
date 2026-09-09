//! `pacode import`: import MCP servers and skills from other coding agents.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::bail;
use pacode_config::Paths;
use pacode_import::{
    ApplyOutcome, ApplyTarget, ImportSource, PlanEntry, apply, check_status, discover,
};

#[cfg(test)]
mod import_cmd_tests;

#[derive(Debug, Default, Clone)]
pub struct ImportArgs {
    pub from: Vec<String>,
    pub mcp: bool,
    pub skills: bool,
    pub apply: bool,
    pub force: bool,
}

pub fn run(args: ImportArgs, paths: &Paths) -> anyhow::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_with_dirs(args, paths, &home, &cwd, &mut std::io::stdout())
}

pub fn run_with_dirs(
    args: ImportArgs,
    paths: &Paths,
    home: &Path,
    cwd: &Path,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let sources = resolve_sources(&args.from)?;

    let (include_mcp, include_skills) = match (args.mcp, args.skills) {
        (true, false) => (true, false),
        (false, true) => (false, true),
        _ => (true, true),
    };

    let discovery = discover(home, cwd, &sources);

    let mut plan = Vec::new();
    if include_mcp {
        for srv in discovery.mcp {
            plan.push(PlanEntry::mcp(srv));
        }
    }
    if include_skills {
        for skill in discovery.skills {
            plan.push(PlanEntry::skill(skill));
        }
    }

    let config_dir = paths.config_file.parent().unwrap_or(Path::new("."));
    let target = ApplyTarget::new(&paths.config_file, config_dir.join("skills"), args.force);

    if !args.apply {
        // DRY RUN: print table
        if plan.is_empty() {
            writeln!(out, "No MCP servers or skills found to import.")?;
        } else {
            writeln!(
                out,
                "{:<12}  {:<6}  {:<24}  {:<9}  DETAIL",
                "SOURCE", "TYPE", "NAME", "STATUS"
            )?;
            for entry in &plan {
                let status = check_status(entry, &target);
                let detail = truncate(&entry.detail(), DETAIL_WIDTH);
                writeln!(
                    out,
                    "{:<12}  {:<6}  {:<24}  {:<9}  {}",
                    entry.source().label(),
                    entry.kind_str(),
                    entry.name(),
                    status.as_str(),
                    detail
                )?;
            }
        }

        if !discovery.errors.is_empty() {
            writeln!(out, "\nWarnings:")?;
            for err in &discovery.errors {
                writeln!(out, "  {err}")?;
            }
        }

        writeln!(out, "\nRe-run with --apply to import.")?;
        return Ok(());
    }

    // APPLY: run write / copy logic
    let report = apply(&plan, &target);

    for entry in &report.entries {
        let kind = entry.kind;
        let name = &entry.name;
        match &entry.outcome {
            ApplyOutcome::Imported => {
                writeln!(out, "Imported {kind} '{name}'")?;
            }
            ApplyOutcome::SkippedConflict => {
                writeln!(
                    out,
                    "Skipped {kind} '{name}' (conflict, use --force to overwrite)"
                )?;
            }
            ApplyOutcome::SkippedSame => {
                writeln!(out, "Skipped {kind} '{name}' (already imported)")?;
            }
            ApplyOutcome::Failed(err) => {
                writeln!(out, "Failed to import {kind} '{name}': {err}")?;
            }
        }
    }

    let srv_count = report.imported_servers;
    let skl_count = report.imported_skills;
    writeln!(
        out,
        "\nImported {srv_count} server(s), {skl_count} skill(s)."
    )?;

    if !discovery.errors.is_empty() {
        writeln!(out, "\nWarnings:")?;
        for err in &discovery.errors {
            writeln!(out, "  {err}")?;
        }
    }

    Ok(())
}

fn resolve_sources(from: &[String]) -> anyhow::Result<Vec<ImportSource>> {
    if from.is_empty() {
        return Ok(ImportSource::all().to_vec());
    }

    let mut sources = Vec::new();
    for item in from {
        let trimmed = item.trim();
        if trimmed.eq_ignore_ascii_case("all") {
            for &s in ImportSource::all() {
                if !sources.contains(&s) {
                    sources.push(s);
                }
            }
            continue;
        }

        let parsed: ImportSource = trimmed.parse().map_err(|_| {
            anyhow::anyhow!(
                "unknown import source '{trimmed}', valid sources: claude, codex, opencode, cursor, gemini, vscode, all"
            )
        })?;
        if !sources.contains(&parsed) {
            sources.push(parsed);
        }
    }

    if sources.is_empty() {
        bail!("no valid import sources specified");
    }

    Ok(sources)
}

/// Columns left for the free-form DETAIL column on a conventional terminal.
const DETAIL_WIDTH: usize = 60;

/// Truncate on a char boundary with an ellipsis; the detail column is a hint,
/// not the payload, and an untruncated skill description wraps the whole table.
fn truncate(text: &str, max: usize) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}
