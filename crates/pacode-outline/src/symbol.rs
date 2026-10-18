//! Symbol lookup: `"bar"`, `"Foo::bar"`, `"Foo.bar"` → the full line range of every
//! matching declaration, so a caller can show a hidden body on request.

use std::ops::Range;

use crate::{Outline, Symbol};

/// One declaration matching a [`find_symbol`] query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolHit {
    /// Named ancestors and the symbol, joined by `::` (`Foo::bar`).
    pub path: String,
    /// Marker word (`fn`, `class`, `impl`).
    pub kind: &'static str,
    /// 0-based rows of the whole declaration, attached doc comments included.
    pub lines: Range<usize>,
}

/// Declarations whose path ends with the query's segments (separators `::`, `.`, `#`
/// and `/`), in file order. Generic arguments are ignored (`impl Foo<T>` matches
/// `Foo`). Exact-case matches win; without any, a case-insensitive match is tried.
/// Unnamed symbols (anonymous functions) are transparent in paths.
pub fn find_symbol(outline: &Outline, query: &str) -> Vec<SymbolHit> {
    let wanted = segments(query);
    if wanted.is_empty() {
        return Vec::new();
    }
    for fold_case in [false, true] {
        let mut hits = Vec::new();
        let mut path = Vec::new();
        collect(&outline.symbols, &wanted, fold_case, &mut path, &mut hits);
        if !hits.is_empty() {
            return hits;
        }
    }
    Vec::new()
}

/// Every named symbol's path, in file order (for "did you mean" lists).
pub fn symbol_paths(outline: &Outline) -> Vec<String> {
    fn walk(symbols: &[Symbol], prefix: &str, out: &mut Vec<String>) {
        for s in symbols {
            let path = match (prefix.is_empty(), s.name.is_empty()) {
                (_, true) => prefix.to_string(),
                (true, false) => s.name.clone(),
                (false, false) => format!("{prefix}::{}", s.name),
            };
            if !s.name.is_empty() {
                out.push(path.clone());
            }
            walk(&s.children, &path, out);
        }
    }
    let mut out = Vec::new();
    walk(&outline.symbols, "", &mut out);
    out
}

fn collect(
    symbols: &[Symbol],
    wanted: &[String],
    fold_case: bool,
    path: &mut Vec<String>,
    hits: &mut Vec<SymbolHit>,
) {
    for symbol in symbols {
        let own = segments(&symbol.name);
        let pushed = own.len();
        path.extend(own);
        if pushed > 0 && ends_with(path, wanted, fold_case) {
            hits.push(SymbolHit {
                path: path.join("::"),
                kind: symbol.kind,
                lines: symbol.lines.clone(),
            });
        }
        collect(&symbol.children, wanted, fold_case, path, hits);
        path.truncate(path.len() - pushed);
    }
}

fn ends_with(path: &[String], wanted: &[String], fold_case: bool) -> bool {
    if wanted.len() > path.len() {
        return false;
    }
    let tail = &path[path.len() - wanted.len()..];
    tail.iter().zip(wanted).all(|(have, want)| {
        if fold_case {
            have.eq_ignore_ascii_case(want)
        } else {
            have == want
        }
    })
}

/// Split a name or query into path segments without generic arguments or whitespace.
fn segments(text: &str) -> Vec<String> {
    let mut plain = String::with_capacity(text.len());
    let mut angle = 0usize;
    for c in text.chars() {
        match c {
            '<' => angle += 1,
            '>' if angle > 0 => angle -= 1,
            _ if angle > 0 => {}
            ':' | '.' | '#' | '/' => plain.push(' '),
            c if c.is_whitespace() => plain.push(' '),
            c => plain.push(c),
        }
    }
    plain
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
