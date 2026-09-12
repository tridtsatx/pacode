//! C-family lexers: Rust, TypeScript/JavaScript, Python and JSON share the
//! skeleton (comments, strings, numbers, identifiers, operators) with
//! per-language details in the word tables and a few specials — Rust raw
//! strings and lifetimes, Python triple quotes and decorators, JS template
//! strings.

use crate::syntax::lexer::{
    Block, Lx, TokenKind, block_comment, ident, is_ident_cont, is_ident_start, line_comment,
    number, quoted,
};

#[cfg(test)]
#[path = "code_tests.rs"]
mod code_tests;

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while",
];

const RUST_TYPES: &[&str] = &[
    "Self", "bool", "char", "str", "String", "i8", "i16", "i32", "i64", "i128", "isize", "u8",
    "u16", "u32", "u64", "u128", "usize", "f32", "f64", "Vec", "Box", "Option", "Result", "Rc",
    "Arc", "Cell", "RefCell", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "VecDeque",
];

const TS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "namespace",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "while",
    "with",
    "yield",
];

const TS_TYPES: &[&str] = &[
    "any", "boolean", "never", "number", "object", "string", "symbol", "unknown", "void", "Array",
    "BigInt", "Boolean", "Map", "Number", "Object", "Promise", "Record", "Set", "String",
];

const PY_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
    "yield", "False", "None", "True",
];

const PY_TYPES: &[&str] = &[
    "bool",
    "bytes",
    "bytearray",
    "complex",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "memoryview",
    "object",
    "range",
    "set",
    "slice",
    "str",
    "tuple",
    "type",
    "BaseException",
    "Exception",
];

const JSON_KEYWORDS: &[&str] = &["false", "null", "true"];

/// Classify an identifier: keyword, `name(`/`name!(` as a function (macros
/// included), a known type name or a `Capitalized` word as a type; plain
/// identifiers stay unhighlighted (`None`).
fn word_kind(
    word: &str,
    lx: &Lx,
    keywords: &[&str],
    types: &[&str],
    caps: bool,
) -> Option<TokenKind> {
    if keywords.contains(&word) {
        return Some(TokenKind::Keyword);
    }
    let next = lx.rest();
    if next.starts_with('!')
        && next[1..]
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '(' | '[' | '{'))
    {
        return Some(TokenKind::Function);
    }
    if next.starts_with('(') {
        return Some(TokenKind::Function);
    }
    if types.contains(&word) || (caps && word.chars().next().is_some_and(char::is_uppercase)) {
        return Some(TokenKind::Type);
    }
    None
}

fn is_op(c: char) -> bool {
    matches!(
        c,
        '=' | '+'
            | '-'
            | '*'
            | '/'
            | '%'
            | '&'
            | '|'
            | '^'
            | '!'
            | '~'
            | '<'
            | '>'
            | '?'
            | ':'
            | ';'
            | ','
            | '.'
            | '@'
            | '#'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
    )
}

/// Rust: `//`, `/* */`, `"` (may span lines), `'a'` chars vs `'a` lifetimes,
/// `r""`/`r#".."#` raw strings, `b""` byte strings.
pub(crate) fn lex_rust(lx: &mut Lx, block: &mut Option<Block>) {
    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if lx.starts("//") {
            line_comment(lx);
        } else if lx.starts("/*") {
            block_comment(lx, "/*", "*/", block);
        } else if c == 'r' && (lx.rest()[1..].starts_with('"') || raw_string_open(lx)) {
            lex_rust_raw(lx, block);
        } else if c == 'b' && lx.rest()[1..].starts_with('"') {
            quoted(lx, "b\"", "\"", true, true, block);
        } else if c == '"' {
            // Rust strings may span lines; an unclosed one continues via block.
            quoted(lx, "\"", "\"", true, true, block);
        } else if c == '\'' {
            rust_tick(lx);
        } else if c.is_ascii_digit() {
            number(lx, false);
        } else if is_ident_start(c) {
            let (s, e) = ident(lx, false);
            if let Some(kind) = word_kind(&lx.line[s..e], lx, RUST_KEYWORDS, RUST_TYPES, true) {
                lx.fill(kind);
            } else {
                lx.skip();
            }
        } else if is_op(c) {
            lx.bump_while(is_op);
            lx.fill(TokenKind::Operator);
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}

/// `r"` / `r#"…"#` — true when `r` introduces a raw string.
fn raw_string_open(lx: &Lx) -> bool {
    let rest = &lx.rest()[1..];
    let hashes = rest.bytes().take_while(|b| *b == b'#').count();
    rest.as_bytes().get(hashes) == Some(&b'"')
}

fn lex_rust_raw(lx: &mut Lx, block: &mut Option<Block>) {
    let rest = &lx.rest()[1..];
    let hashes = rest.bytes().take_while(|b| *b == b'#').count();
    // `r` + `#`*n + `"`
    lx.bump(1 + hashes + 1);
    let end = format!("\"{}", "#".repeat(hashes));
    if lx.eat_until(&end, false) {
        lx.fill(TokenKind::String);
    } else {
        lx.fill(TokenKind::String);
        *block = Some(Block::Until {
            end,
            kind: TokenKind::String,
            escape: false,
        });
    }
}

/// `'x'` / `'\n'` is a char literal (String); `'a` without a closing quote is a
/// lifetime (Type).
fn rust_tick(lx: &mut Lx) {
    let start = lx.pos;
    lx.bump(1);
    match lx.peek() {
        Some('\\') => {
            lx.bump(1);
            lx.bump_while(|c| c != '\'');
            if lx.peek() == Some('\'') {
                lx.bump(1);
            }
            lx.fill(TokenKind::String);
        }
        Some(c) if is_ident_cont(c) => {
            lx.bump_while(is_ident_cont);
            if lx.peek() == Some('\'') && lx.pos - start <= 4 {
                lx.bump(1);
                lx.fill(TokenKind::String);
            } else {
                lx.fill(TokenKind::Type);
            }
        }
        _ => {
            lx.fill(TokenKind::Operator);
        }
    }
}

/// TypeScript/JavaScript: `//`, `/* */`, `'`/`"` strings, and `` ` `` template
/// strings that may span lines (block until the next backtick — `${}` nesting
/// is deliberately not modelled).
pub(crate) fn lex_tsjs(lx: &mut Lx, block: &mut Option<Block>) {
    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if lx.starts("//") {
            line_comment(lx);
        } else if lx.starts("/*") {
            block_comment(lx, "/*", "*/", block);
        } else if c == '`' {
            quoted(lx, "`", "`", true, true, block);
        } else if c == '"' {
            quoted(lx, "\"", "\"", true, false, block);
        } else if c == '\'' {
            quoted(lx, "'", "'", true, false, block);
        } else if c.is_ascii_digit() {
            number(lx, false);
        } else if is_ident_start(c) || c == '$' {
            let s = lx.pos;
            if c == '$' {
                lx.bump(1);
            }
            let (_, e) = ident(lx, false);
            if let Some(kind) = word_kind(&lx.line[s..e], lx, TS_KEYWORDS, TS_TYPES, true) {
                lx.fill(kind);
            } else {
                lx.skip();
            }
        } else if is_op(c) {
            lx.bump_while(is_op);
            lx.fill(TokenKind::Operator);
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}

/// Python: `#` comments, `'`/`"` strings with `rbfu` prefixes, `'''`/`"""`
/// block strings, `@decorator` as a function token.
pub(crate) fn lex_python(lx: &mut Lx, block: &mut Option<Block>) {
    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if c == '#' {
            line_comment(lx);
        } else if c == '@' && lx.rest()[1..].starts_with(is_ident_start) {
            lx.bump(1);
            ident(lx, false);
            lx.fill(TokenKind::Function);
        } else if py_string_open(lx) {
            lex_python_string(lx, block);
        } else if c.is_ascii_digit() {
            number(lx, false);
        } else if is_ident_start(c) {
            let (s, e) = ident(lx, false);
            if let Some(kind) = word_kind(&lx.line[s..e], lx, PY_KEYWORDS, PY_TYPES, true) {
                lx.fill(kind);
            } else {
                lx.skip();
            }
        } else if is_op(c) {
            lx.bump_while(is_op);
            lx.fill(TokenKind::Operator);
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}

/// A quote possibly preceded by a `rbfu` prefix (`rb"`, `F'`, …).
fn py_string_open(lx: &Lx) -> bool {
    let bytes = lx.rest().as_bytes();
    let mut i = 0;
    while i < 2
        && i < bytes.len()
        && matches!(bytes[i].to_ascii_lowercase(), b'r' | b'b' | b'f' | b'u')
    {
        i += 1;
    }
    matches!(bytes.get(i), Some(b'"' | b'\''))
}

fn lex_python_string(lx: &mut Lx, block: &mut Option<Block>) {
    // Skip a 1–2 char literal prefix, then open on the quote.
    let bytes = lx.rest().as_bytes();
    let mut i = 0;
    while i < 2
        && i < bytes.len()
        && matches!(bytes[i].to_ascii_lowercase(), b'r' | b'b' | b'f' | b'u')
    {
        i += 1;
    }
    lx.bump(i);
    let q = match lx.peek() {
        Some(q @ ('"' | '\'')) => q,
        _ => return,
    };
    if lx.rest().starts_with(&format!("{q}{q}{q}")) {
        let open = format!("{q}{q}{q}");
        quoted(lx, &open, &open, true, true, block);
    } else {
        let open = q.to_string();
        quoted(lx, &open, &open, true, false, block);
    }
}

/// JSON: `"` strings, numbers, `true/false/null`, punctuation as operators.
/// Keys are strings too — no separate kind exists for them.
pub(crate) fn lex_json(lx: &mut Lx, block: &mut Option<Block>) {
    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if c == '"' {
            quoted(lx, "\"", "\"", true, false, block);
        } else if c.is_ascii_digit()
            || (c == '-' && lx.rest()[1..].starts_with(|d: char| d.is_ascii_digit()))
        {
            number(lx, true);
        } else if is_ident_start(c) {
            let (s, e) = ident(lx, false);
            if JSON_KEYWORDS.contains(&&lx.line[s..e]) {
                lx.fill(TokenKind::Keyword);
            } else {
                lx.skip();
            }
        } else if matches!(c, '{' | '}' | '[' | ']' | ':' | ',') {
            lx.bump(c.len_utf8());
            lx.fill(TokenKind::Operator);
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}
