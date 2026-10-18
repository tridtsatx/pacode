//! Parsing with a wall-clock timeout, and the syntax-error ratio check.
//!
//! No parser is cached: `Parser::new` + `set_language` cost microseconds next to the
//! parse itself, and a fresh parser per call leaves nothing to size, own or invalidate.

use std::ops::ControlFlow;
use std::time::Instant;

use tree_sitter::{Node, ParseOptions, ParseState, Parser, Point, Tree};

use crate::{Lang, OutlineError, OutlineOpts, Unsupported};

/// Parse `source`; the progress callback aborts the parse after `opts.parse_timeout`
/// (tree-sitter 0.26 removed `Parser::set_timeout_micros`; this is its replacement).
pub(crate) fn parse(source: &str, lang: Lang, opts: &OutlineOpts) -> Result<Tree, OutlineError> {
    let unsupported = |reason| OutlineError::Unsupported {
        lang: lang.name(),
        reason,
    };
    let language = lang
        .grammar()
        .ok_or_else(|| unsupported(Unsupported::NotCompiled))?;
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| unsupported(Unsupported::GrammarVersion(language.abi_version())))?;

    let started = Instant::now();
    let timeout = opts.parse_timeout;
    let mut progress = |_: &ParseState| {
        if started.elapsed() > timeout {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let bytes = source.as_bytes();
    let mut input = |offset: usize, _: Point| bytes.get(offset..).unwrap_or_default();
    // `None` only when the callback stopped the parse (the language is set).
    parser
        .parse_with_options(&mut input, None, Some(options))
        .ok_or(OutlineError::Timeout {
            ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        })
}

/// Percentage of nodes that are syntax errors: `ERROR` nodes with everything under
/// them, plus `MISSING` tokens. Walks the whole tree only when it has an error at all;
/// each node costs one unit of `budget`.
pub(crate) fn error_percent(root: Node<'_>, budget: usize) -> Result<u32, OutlineError> {
    if !root.has_error() {
        return Ok(0);
    }
    let mut cursor = root.walk();
    let (mut total, mut bad) = (0usize, 0usize);
    let mut depth = 0usize;
    // Depth of the ERROR node we are inside, if any.
    let mut in_error: Option<usize> = None;
    loop {
        let node = cursor.node();
        total += 1;
        if total > budget {
            return Err(OutlineError::Budget { budget });
        }
        if in_error.is_some() || node.is_missing() {
            bad += 1;
        } else if node.is_error() {
            bad += 1;
            in_error = Some(depth);
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(percent(bad, total));
            }
            depth = depth.saturating_sub(1);
        }
        // Now at a sibling on `depth`: we left the error subtree if it began at or below it.
        if in_error.is_some_and(|d| depth <= d) {
            in_error = None;
        }
    }
}

fn percent(bad: usize, total: usize) -> u32 {
    let pct = bad.saturating_mul(100) / total.max(1);
    u32::try_from(pct).unwrap_or(100)
}
