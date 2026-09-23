//! Symbol extraction over a parsed file: query evaluation, containment-based
//! parenting, kind reclassification and dedup.
//!
//! `Parser` is `!Sync` and parse is CPU-bound — callers are expected to run
//! [`extract`] inside `spawn_blocking` (the `symbols` tool does exactly that).

use std::time::Instant;

use tree_sitter::{ParseOptions, ParseState, Parser, Query, QueryCursor, StreamingIterator};

use crate::language::Language;
use crate::queries;

/// Sources larger than this are refused. Enforced here (not in the tool
/// wrapper) so every caller — tool, tests, future consumers — shares one bound.
pub const MAX_PARSE_BYTES: usize = 2 * 1024 * 1024;

/// Per-file parse budget. Long enough for real-world files, short enough that a
/// pathological grammar input cannot wedge a tool call.
const PARSE_TIMEOUT_MICROS: u64 = 100_000;

#[derive(Debug, thiserror::Error)]
pub enum SymbolsError {
    #[error("source too large: {size} bytes (max {MAX_PARSE_BYTES})")]
    TooLarge { size: usize },
    #[error("failed to initialize the {lang} grammar: {reason}")]
    LanguageInit { lang: Language, reason: String },
    #[error("invalid symbol query for {lang}: {source}")]
    Query {
        lang: Language,
        #[source]
        source: tree_sitter::QueryError,
    },
    /// Parsing was cancelled (timeout) or the grammar produced no tree.
    #[error("tree-sitter produced no syntax tree for {lang}")]
    NoTree { lang: Language },
}

/// Normalized symbol categories across all grammars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Fn,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Impl,
    Interface,
    Const,
    Type,
    Module,
    Export,
    Macro,
}

impl SymbolKind {
    /// Stable lowercase token — used in query capture names, the `kinds`
    /// filter and the output format.
    pub fn name(self) -> &'static str {
        match self {
            SymbolKind::Fn => "fn",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Interface => "interface",
            SymbolKind::Const => "const",
            SymbolKind::Type => "type",
            SymbolKind::Module => "module",
            SymbolKind::Export => "export",
            SymbolKind::Macro => "macro",
        }
    }

    /// Parse a [`SymbolKind::name`] token (the `kinds` tool argument).
    pub fn from_name(name: &str) -> Option<SymbolKind> {
        match name.trim().to_ascii_lowercase().as_str() {
            "fn" | "func" | "function" => Some(SymbolKind::Fn),
            "method" => Some(SymbolKind::Method),
            "class" => Some(SymbolKind::Class),
            "struct" => Some(SymbolKind::Struct),
            "enum" => Some(SymbolKind::Enum),
            "trait" => Some(SymbolKind::Trait),
            "impl" => Some(SymbolKind::Impl),
            "interface" => Some(SymbolKind::Interface),
            "const" | "constant" | "var" => Some(SymbolKind::Const),
            "type" => Some(SymbolKind::Type),
            "module" | "mod" | "namespace" => Some(SymbolKind::Module),
            "export" => Some(SymbolKind::Export),
            "macro" => Some(SymbolKind::Macro),
            _ => None,
        }
    }

    /// All kinds, in display order.
    pub const ALL: &'static [SymbolKind] = &[
        SymbolKind::Fn,
        SymbolKind::Method,
        SymbolKind::Class,
        SymbolKind::Struct,
        SymbolKind::Enum,
        SymbolKind::Trait,
        SymbolKind::Impl,
        SymbolKind::Interface,
        SymbolKind::Const,
        SymbolKind::Type,
        SymbolKind::Module,
        SymbolKind::Export,
        SymbolKind::Macro,
    ];
}

/// One extracted symbol. Lines are 1-based and inclusive on both ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// Display name — qualified where trivially derivable (`Server.Start`,
    /// `Trait for Type`, `Foo::bar`).
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based inclusive start line of the defining node.
    pub start_line: u32,
    /// 1-based inclusive end line of the defining node.
    pub end_line: u32,
    /// Index of the containing symbol in the same [`FileSymbols::symbols`]
    /// vector (e.g. the impl/class/module a method is nested in).
    pub parent: Option<usize>,
}

impl Symbol {
    /// Nesting depth: 0 for top-level symbols, `parent`'s depth + 1 otherwise.
    /// Parents always precede children in `all`, so the walk terminates.
    pub fn depth(&self, all: &[Symbol]) -> usize {
        let mut depth = 0;
        let mut next = self.parent;
        while let Some(idx) = next {
            depth += 1;
            next = all.get(idx).and_then(|s| s.parent);
        }
        depth
    }
}

/// Symbols extracted from one source file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSymbols {
    /// The syntax tree contains error nodes; `symbols` is best-effort.
    pub parsed_with_errors: bool,
    /// First 1-based line holding a syntax error, when `parsed_with_errors`.
    pub first_error_line: Option<u32>,
    pub symbols: Vec<Symbol>,
}

/// Extraction tuning knobs; `Default` is the production configuration.
#[derive(Clone, Copy, Debug)]
pub struct ExtractOptions {
    /// Per-file parse budget in microseconds.
    pub timeout_micros: u64,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            timeout_micros: PARSE_TIMEOUT_MICROS,
        }
    }
}

/// A raw query match before parenting/dedup.
struct Candidate {
    kind: SymbolKind,
    name: String,
    /// Byte range of the kind-captured node — used for containment and dedup.
    start_byte: usize,
    end_byte: usize,
    /// 1-based inclusive line range shown in the outline.
    start_line: u32,
    end_line: u32,
    /// Pattern index in the query source — tie-breaks same-(name, start)
    /// collisions toward earlier, more specific patterns.
    pattern: usize,
}

/// Parse `source` as `lang` and return its symbol outline.
///
/// A fresh `Parser` is created per call (`Parser` is `!Sync`). Parsing aborts
/// via a progress callback once `opts.timeout_micros` elapse. A tree with
/// error nodes still produces symbols — `parsed_with_errors` /
/// `first_error_line` report the damage instead of hiding it behind an empty
/// result.
pub fn extract(
    source: &str,
    lang: Language,
    opts: ExtractOptions,
) -> Result<FileSymbols, SymbolsError> {
    if source.len() > MAX_PARSE_BYTES {
        return Err(SymbolsError::TooLarge { size: source.len() });
    }
    if source.is_empty() {
        return Ok(FileSymbols::default());
    }

    let ts_lang = lang.ts_language();
    let mut parser = Parser::new();
    parser
        .set_language(&ts_lang)
        .map_err(|e| SymbolsError::LanguageInit {
            lang,
            reason: e.to_string(),
        })?;

    let started = Instant::now();
    let timeout = opts.timeout_micros;
    // `true` from the progress callback cancels the parse.
    let mut on_progress = move |_: &ParseState| started.elapsed().as_micros() as u64 > timeout;
    let bytes = source.as_bytes();
    let tree = parser
        .parse_with_options(
            &mut |offset, _| {
                if offset < bytes.len() {
                    &bytes[offset..]
                } else {
                    &bytes[0..0]
                }
            },
            None,
            Some(ParseOptions::new().progress_callback(&mut on_progress)),
        )
        .ok_or(SymbolsError::NoTree { lang })?;
    let root = tree.root_node();

    let query = Query::new(&ts_lang, queries::query_src(lang))
        .map_err(|source| SymbolsError::Query { lang, source })?;

    let candidates = collect_candidates(bytes, root, &query, lang);
    let symbols = build_symbols(candidates, lang);

    let parsed_with_errors = root.has_error();
    let first_error_line = parsed_with_errors.then(|| first_error_line(root));
    Ok(FileSymbols {
        parsed_with_errors,
        first_error_line,
        symbols,
    })
}

/// Run the query and fold matches into candidates.
fn collect_candidates(
    source: &[u8],
    root: tree_sitter::Node<'_>,
    query: &Query,
    lang: Language,
) -> Vec<Candidate> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, source);

    // Resolve capture indices once against the query's capture-name table:
    // `kind_of[i]` is the kind for capture i when it is a `@<kind>` capture.
    let capture_names = query.capture_names();
    let mut kind_of: Vec<Option<SymbolKind>> = vec![None; capture_names.len()];
    for kind in SymbolKind::ALL {
        if let Some(idx) = capture_names.iter().position(|n| *n == kind.name()) {
            kind_of[idx] = Some(*kind);
        }
    }
    let name_idx = capture_names.iter().position(|n| *n == "name");
    let container_idx = capture_names.iter().position(|n| *n == "container");

    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let mut span = None;
        let mut kind = None;
        let mut name_lo = usize::MAX;
        let mut name_hi = 0usize;
        let mut container: Option<tree_sitter::Node> = None;

        for capture in m.captures {
            let idx = capture.index as usize;
            let node = capture.node;
            if Some(idx) == name_idx {
                name_lo = name_lo.min(node.start_byte());
                name_hi = name_hi.max(node.end_byte());
            } else if Some(idx) == container_idx {
                container = Some(node);
            } else if let Some(k) = kind_of.get(idx).copied().flatten() {
                span = Some(node);
                kind = Some(k);
            }
        }

        let (Some(node), Some(kind)) = (span, kind) else {
            continue;
        };

        // Multiple `@name` captures merge into the contiguous range covering
        // them: `impl Trait for X` binds trait+type → `Trait for X`.
        let name = if name_lo <= name_hi {
            let mut text = String::from_utf8_lossy(&source[name_lo..name_hi]).into_owned();
            if let Some(container) = container {
                let prefix =
                    String::from_utf8_lossy(&source[container.start_byte()..container.end_byte()]);
                text = format!("{prefix}{}{text}", queries::name_sep(lang));
            }
            text
        } else {
            queries::fallback_name(lang, node.kind())
        };

        out.push(Candidate {
            kind,
            name,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_line: node.start_position().row as u32 + 1,
            end_line: content_end_line(node),
            pattern: m.pattern_index,
        });
    }
    out
}

/// Last line (1-based, inclusive) holding a content byte of `node`.
///
/// `end_position` is one past the last byte; when it lands at column 0 of a
/// fresh row the last content byte sits on the row above.
fn content_end_line(node: tree_sitter::Node<'_>) -> u32 {
    let pos = node.end_position();
    let row = if pos.column == 0 && pos.row > 0 {
        pos.row - 1
    } else {
        pos.row
    };
    row as u32 + 1
}

/// First line holding an `ERROR` or missing node (depth-first, so the lowest
/// row number in document order).
fn first_error_line(node: tree_sitter::Node<'_>) -> u32 {
    let mut best = u32::MAX;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.is_error() || current.is_missing() {
            best = best.min(current.start_position().row as u32 + 1);
        }
        for i in (0..current.child_count()).rev() {
            if let Some(child) = current.child(i) {
                stack.push(child);
            }
        }
    }
    if best == u32::MAX {
        node.start_position().row as u32 + 1
    } else {
        best
    }
}

/// Sort, dedup, resolve parents and reclassify `fn` → `method`.
fn build_symbols(mut candidates: Vec<Candidate>, lang: Language) -> Vec<Symbol> {
    // Order by start; on identical starts the earlier query pattern wins.
    candidates.sort_by_key(|c| (c.start_byte, c.pattern));

    // Dedup pass (`kept` stays start-sorted):
    // - same name + same start, different kind → earlier pattern wins
    //   (Go `type_spec` constrained to `struct_type` beats generic `@type`);
    // - same name + same kind + same start → earlier pattern wins
    //   (the spec's `(name, kind, start)` dedup key);
    // - same name + same kind, contained range → the outer span wins
    //   (`decorated_definition` swallows the inner `function_definition`;
    //   outer always sorts first since its start is no greater);
    // - same kind + identical span, different names → earlier pattern wins
    //   (a named variant of an operator/ctor beats the nameless fallback).
    let mut kept: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for cand in candidates {
        let dominated = kept.iter().any(|prev| {
            let same_span = prev.start_byte == cand.start_byte && prev.end_byte == cand.end_byte;
            let same_start = prev.start_byte == cand.start_byte;
            (prev.name == cand.name
                && prev.start_byte <= cand.start_byte
                && prev.end_byte >= cand.end_byte
                && (prev.kind == cand.kind || same_start))
                || (prev.kind == cand.kind && same_span)
                || (prev.name == cand.name && prev.kind == cand.kind && same_start)
        });
        if !dominated {
            kept.push(cand);
        }
    }

    // Parent resolution: parents precede children (start-sorted, strict
    // containment), and AST spans never partially overlap — a single stack of
    // still-open spans finds each symbol's nearest container in one pass.
    let method_parents = queries::method_parents(lang);
    let mut symbols: Vec<Symbol> = Vec::with_capacity(kept.len());
    // (start_byte, end_byte, symbol index) of enclosing candidates.
    let mut open: Vec<(usize, usize, usize)> = Vec::new();
    for cand in kept {
        while let Some(&(p_start, p_end, _)) = open.last() {
            let contains = p_start <= cand.start_byte
                && p_end >= cand.end_byte
                && (p_start != cand.start_byte || p_end != cand.end_byte);
            if contains {
                break;
            }
            open.pop();
        }

        let parent = open.last().map(|&(_, _, idx)| idx);
        let mut kind = cand.kind;
        if kind == SymbolKind::Fn
            && let Some(p) = parent
            && method_parents.contains(&symbols[p].kind)
        {
            kind = SymbolKind::Method;
        }
        symbols.push(Symbol {
            name: cand.name,
            kind,
            start_line: cand.start_line,
            end_line: cand.end_line,
            parent,
        });
        open.push((cand.start_byte, cand.end_byte, symbols.len() - 1));
    }
    symbols
}
