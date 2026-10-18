//! Structural outlines of source files, for reading large files cheaply.
//!
//! [`outline`] parses a file with tree-sitter and keeps, for every declaration, its
//! signature, the first [`OutlineOpts::body_lines`] lines of its body and its closing
//! line; containers (classes, impls, modules) keep their header and closing line and
//! outline each member. Everything else is reported as [`Hidden`] row ranges with a
//! label (`fn foo`, `impl Bar`, `imports`) so a caller can print a marker per region
//! and expand it on request ([`find_symbol`] or an explicit range).
//!
//! The walk is language-agnostic; each language is a static table of node kinds
//! (`tables/*.rs`). Grammars are Cargo features (default: all), so a build can drop
//! the large ones; [`Lang::is_available`] reports what is compiled in.
//!
//! Pure and synchronous: no I/O, no threads, no global state. Caps: input size
//! ([`OutlineOpts::max_input_bytes`]), parse time ([`OutlineOpts::parse_timeout`]) and
//! visited nodes ([`OutlineOpts::node_budget`]). Nothing is cached between calls.

mod build;
mod lang;
mod names;
mod nodes;
mod parse;
mod spec;
mod symbol;
mod tables;
mod walk;

use std::ops::Range;
use std::time::Duration;

pub use lang::Lang;
pub use symbol::{SymbolHit, find_symbol, symbol_paths};

/// Default cap on the input size.
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

/// Knobs of [`outline`]; [`Default`] is what the `read` tool uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineOpts {
    /// Body lines kept after a signature (and per collapsed binding / attribute).
    pub body_lines: usize,
    /// Gaps shorter than this many lines are shown instead of hidden.
    pub min_hidden: usize,
    /// Members of one container outlined before the rest is hidden in one region
    /// (data formats use at most 100).
    pub max_members: usize,
    /// Lines kept of a body-less declaration (trait method, prototype, macro).
    pub max_signature_lines: usize,
    /// Larger inputs are refused ([`OutlineError::TooLarge`]).
    pub max_input_bytes: usize,
    /// Wall-clock cap on parsing ([`OutlineError::Timeout`]).
    pub parse_timeout: Duration,
    /// Cap on nodes visited by the error check and the walk ([`OutlineError::Budget`]).
    pub node_budget: usize,
    /// Parses with more than this percentage of error nodes are refused
    /// ([`Unsupported::SyntaxErrors`]).
    pub max_error_percent: u32,
}

impl Default for OutlineOpts {
    fn default() -> Self {
        Self {
            body_lines: 3,
            min_hidden: 3,
            max_members: 500,
            max_signature_lines: 12,
            max_input_bytes: MAX_INPUT_BYTES,
            parse_timeout: Duration::from_millis(200),
            node_budget: 2_000_000,
            max_error_percent: 30,
        }
    }
}

/// The outline of one file. `kept` and `hidden` are sorted, disjoint, and together
/// cover `0..total_lines` (rows are 0-based line indexes, as `str::lines` counts them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outline {
    pub total_lines: usize,
    pub kept: Vec<Range<usize>>,
    pub hidden: Vec<Hidden>,
    /// Declarations as a tree (containers hold their members).
    pub symbols: Vec<Symbol>,
}

/// A run of hidden rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hidden {
    pub lines: Range<usize>,
    /// What the rows belong to (`fn foo`, `impl Bar`, `imports`); `None` for top-level code.
    pub label: Option<String>,
}

/// A declaration found by the walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// Marker word: `fn`, `impl`, `class`, `const`...
    pub kind: &'static str,
    /// Declared name (`Foo`, `A::g`); empty for anonymous functions / objects.
    pub name: String,
    /// 0-based rows of the whole declaration, attached doc comments and attributes included.
    pub lines: Range<usize>,
    pub children: Vec<Symbol>,
}

/// Shown and hidden ranges in file order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Shown(&'a Range<usize>),
    Hidden(&'a Hidden),
}

impl Outline {
    /// Number of shown lines.
    pub fn shown_lines(&self) -> usize {
        self.kept.iter().map(ExactSizeIterator::len).sum()
    }

    /// Shown and hidden ranges merged in file order.
    pub fn segments(&self) -> Vec<Segment<'_>> {
        let mut out: Vec<Segment<'_>> = self
            .kept
            .iter()
            .map(Segment::Shown)
            .chain(self.hidden.iter().map(Segment::Hidden))
            .collect();
        out.sort_by_key(|s| match s {
            Segment::Shown(r) => r.start,
            Segment::Hidden(h) => h.lines.start,
        });
        out
    }
}

/// Why a language cannot be outlined; the caller falls back to a plain read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unsupported {
    #[error("its grammar is not compiled into this build")]
    NotCompiled,
    #[error("its grammar has ABI version {0}, which this tree-sitter cannot load")]
    GrammarVersion(usize),
    #[error("{percent}% of the syntax tree is parse errors")]
    SyntaxErrors { percent: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutlineError {
    #[error("no {lang} outline: {reason}")]
    Unsupported {
        lang: &'static str,
        reason: Unsupported,
    },
    #[error("file too large for an outline ({bytes} bytes, cap {cap})")]
    TooLarge { bytes: usize, cap: usize },
    #[error("parsing took longer than {ms} ms")]
    Timeout { ms: u64 },
    #[error("outline gave up after visiting {budget} syntax nodes")]
    Budget { budget: usize },
}

/// Outline `source` as `lang`. Fails (and the caller should show the plain file) when
/// the grammar is not compiled in, the input is over the size cap, parsing times out,
/// the node budget runs out, or more than `max_error_percent` of the tree is errors.
pub fn outline(source: &str, lang: Lang, opts: &OutlineOpts) -> Result<Outline, OutlineError> {
    if source.len() > opts.max_input_bytes {
        return Err(OutlineError::TooLarge {
            bytes: source.len(),
            cap: opts.max_input_bytes,
        });
    }
    let tree = parse::parse(source, lang, opts)?;
    let root = tree.root_node();
    let percent = parse::error_percent(root, opts.node_budget)?;
    if percent > opts.max_error_percent {
        return Err(OutlineError::Unsupported {
            lang: lang.name(),
            reason: Unsupported::SyntaxErrors { percent },
        });
    }
    let rows = source.lines().count();
    let mut walker = walk::Walker::new(source, rows, lang.spec(), opts);
    let symbols = walker.members(root, 0)?;
    let (keep, hints) = walker.into_parts();
    Ok(build::assemble(
        source,
        keep,
        &hints,
        symbols,
        opts.min_hidden,
    ))
}

#[cfg(test)]
#[path = "outline_tests.rs"]
mod outline_tests;

#[cfg(test)]
#[path = "lang_tests.rs"]
mod lang_tests;

#[cfg(test)]
#[path = "symbol_tests.rs"]
mod symbol_tests;
