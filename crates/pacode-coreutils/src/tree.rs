//! `tree [-L N] [-a] [-d] [--noreport] [DIR]`: depth-limited, gitignore-aware listing.

use std::ffi::OsStr;
use std::path::Path;

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::{io_error, resolve};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tree_tests;

const DEFAULT_DEPTH: usize = 3;
const MAX_DEPTH: usize = 12;
const MAX_ENTRIES: usize = 5000;

struct Node {
    depth: usize,
    name: String,
    is_dir: bool,
}

/// Mark which nodes are the last child of their parent (nodes are in DFS order).
fn last_flags(nodes: &[Node]) -> Vec<bool> {
    let mut is_last = vec![false; nodes.len()];
    let mut seen: Vec<bool> = vec![false; MAX_DEPTH + 2];
    for (i, node) in nodes.iter().enumerate().rev() {
        let d = node.depth.min(MAX_DEPTH + 1);
        is_last[i] = !seen[d];
        seen[d] = true;
        for deeper in seen.iter_mut().skip(d + 1) {
            *deeper = false;
        }
    }
    is_last
}

/// `tree` over `ignore::WalkBuilder` (hidden files and .gitignore honoured, sorted by name).
pub fn tree(args: &[String], _stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        val("L", 'L', "level"),
        sw("a", 'a', "all"),
        sw("d", 'd', "dirs-only"),
        sw("noreport", '\0', "noreport"),
    ];
    let p = parse(args, SPEC, false)?;
    if p.operands.len() > 1 {
        return Err(UtilError::usage("expected at most one directory"));
    }
    let depth = match p.get("L") {
        Some(l) => {
            let n = parse_usize("level", l)?;
            if n == 0 {
                return Err(UtilError::usage("level must be at least 1"));
            }
            n.min(MAX_DEPTH)
        }
        None => DEFAULT_DEPTH,
    };
    let shown_root = p
        .operands
        .first()
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let root = resolve(cwd, &shown_root);
    let meta = std::fs::metadata(&root).map_err(|e| io_error(&shown_root, &e))?;
    if !meta.is_dir() {
        return Err(UtilError::failed(format!("{shown_root}: not a directory")));
    }
    let show_all = p.has("a");
    let dirs_only = p.has("d");

    let mut builder = ignore::WalkBuilder::new(&root);
    builder
        .max_depth(Some(depth))
        .hidden(!show_all)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b));
    if show_all {
        builder.filter_entry(|entry| entry.file_name() != OsStr::new(".git"));
    }

    let mut nodes: Vec<Node> = Vec::new();
    let mut truncated = false;
    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if dirs_only && !is_dir {
            continue;
        }
        if nodes.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        nodes.push(Node {
            depth: entry.depth(),
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }

    let is_last = last_flags(&nodes);
    let mut out = format!("{shown_root}\n");
    let mut ancestors: Vec<bool> = Vec::new();
    let (mut dirs, mut files) = (0usize, 0usize);
    for (i, node) in nodes.iter().enumerate() {
        ancestors.truncate(node.depth - 1);
        for last in &ancestors {
            out.push_str(if *last { "    " } else { "│   " });
        }
        out.push_str(if is_last[i] {
            "└── "
        } else {
            "├── "
        });
        out.push_str(&node.name);
        out.push('\n');
        ancestors.push(is_last[i]);
        if node.is_dir {
            dirs += 1;
        } else {
            files += 1;
        }
    }
    if truncated {
        out.push_str(&format!("... (stopped after {MAX_ENTRIES} entries)\n"));
    }
    if !p.has("noreport") {
        if dirs_only {
            out.push_str(&format!("\n{dirs} directories\n"));
        } else {
            out.push_str(&format!("\n{dirs} directories, {files} files\n"));
        }
    }
    Ok(UtilOutput::ok(out))
}
