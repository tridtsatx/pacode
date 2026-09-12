//! Lexers for the non-C languages: TOML, YAML, Bash and Markdown. Same token
//! contract as `code.rs` — the line is tiled end to end, `None` is plain text.

use crate::syntax::lexer::{
    Block, Lx, TokenKind, ident, is_ident_cont, is_ident_start, line_comment, number, quoted,
};

#[cfg(test)]
#[path = "misc_tests.rs"]
mod misc_tests;

const TOML_KEYWORDS: &[&str] = &["false", "true"];

const YAML_KEYWORDS: &[&str] = &[
    "false", "no", "null", "off", "on", "true", "yes", "~", "Null", "TRUE", "FALSE", "Yes", "No",
    "On", "Off",
];

const BASH_KEYWORDS: &[&str] = &[
    "case", "coproc", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in",
    "select", "then", "time", "until", "while",
];

/// Columns of leading whitespace; a tab counts as the next multiple of 4,
/// close enough for YAML scalar indentation.
pub(crate) fn leading_indent(line: &str) -> usize {
    let mut n = 0usize;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4 - n % 4,
            _ => break,
        }
    }
    n
}

/// True when only whitespace precedes the cursor on this line.
fn at_line_start(lx: &Lx) -> bool {
    lx.line[..lx.pos].trim().is_empty()
}

/// TOML: `#` comments, `'`/`"`/`'''`/`"""` strings, `[section]` headers as
/// types, `key =` as a function, booleans as keywords, numbers and dates.
pub(crate) fn lex_toml(lx: &mut Lx, block: &mut Option<Block>) {
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
        } else if lx.starts("\"\"\"") {
            quoted(lx, "\"\"\"", "\"\"\"", true, true, block);
        } else if lx.starts("'''") {
            quoted(lx, "'''", "'''", false, true, block);
        } else if c == '"' {
            quoted(lx, "\"", "\"", true, false, block);
        } else if c == '\'' {
            quoted(lx, "'", "'", false, false, block);
        } else if c == '[' && at_line_start(lx) {
            // `[table]` / `[[array]]` header — a type up to the closing bracket.
            lx.bump(1);
            if lx.eat_until("]", false) && lx.peek() == Some(']') {
                lx.bump(1);
            }
            lx.fill(TokenKind::Type);
        } else if c.is_ascii_digit() {
            number(lx, true);
        } else if is_ident_start(c) || c == '-' {
            let (s, e) = ident(lx, true);
            if s == e {
                lx.bump(c.len_utf8());
                lx.fill(TokenKind::Operator);
                continue;
            }
            let word = &lx.line[s..e];
            if TOML_KEYWORDS.contains(&word) {
                lx.fill(TokenKind::Keyword);
            } else if lx.rest().trim_start().starts_with('=') && !lx.line[..s].contains('=') {
                // A bare key to the left of the first `=` is a name.
                lx.fill(TokenKind::Function);
            } else {
                lx.skip();
            }
        } else if matches!(c, '=' | '.' | ',' | '{' | '}' | '[' | ']') {
            lx.bump(c.len_utf8());
            lx.fill(TokenKind::Operator);
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}

/// YAML: `#` comments at word boundaries, `'`/`"` strings, `key:` names as
/// functions, `-`/`?`/anchors/directives as operators, `|`/`>` block scalars
/// that own the following indented lines.
pub(crate) fn lex_yaml(lx: &mut Lx, block: &mut Option<Block>) {
    let line_indent = leading_indent(lx.line);
    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if c == '#'
            && (lx.pos == 0
                || lx
                    .prev_byte()
                    .is_some_and(|b| matches!(b, b' ' | b'\t' | b':')))
        {
            line_comment(lx);
        } else if (lx.starts("---") || lx.starts("...")) && at_line_start(lx) {
            lx.bump_while(|c| c == '-' || c == '.');
            lx.fill(TokenKind::Operator);
        } else if lx.starts("|") || lx.starts(">") {
            // Block scalar header — but only when nothing else follows on the
            // line (a `|` inside a plain scalar stays a plain operator).
            let save = lx.pos;
            lx.bump_while(|c| matches!(c, '|' | '>' | '-' | '+' | '0'..='9'));
            let tail = lx.rest().trim_start();
            if tail.is_empty() || tail.starts_with('#') {
                lx.fill(TokenKind::Operator);
                *block = Some(Block::YamlScalar {
                    indent: line_indent,
                });
            } else {
                lx.pos = save;
                lx.bump(1);
                lx.fill(TokenKind::Operator);
            }
        } else if c == '"' {
            quoted(lx, "\"", "\"", true, false, block);
        } else if c == '\'' {
            // YAML escapes '' inside single quotes; eat_until handles that
            // poorly, so scan a doubled-quote aware loop instead.
            yaml_single_quoted(lx);
        } else if c == '-' && lx.rest()[1..].starts_with(' ') || c == '-' && lx.rest().len() == 1 {
            lx.bump(1);
            lx.fill(TokenKind::Operator);
        } else if matches!(c, '&' | '*' | '!') {
            lx.bump(1);
            lx.bump_while(is_ident_cont);
            lx.fill(TokenKind::Operator);
        } else if c == '%' && at_line_start(lx) {
            lx.bump_while(|c| !c.is_whitespace());
            lx.fill(TokenKind::Operator);
        } else if c.is_ascii_digit()
            || (c == '-' && lx.rest()[1..].starts_with(|d: char| d.is_ascii_digit()))
        {
            number(lx, true);
        } else if is_ident_start(c) {
            let (s, e) = ident(lx, true);
            let word = &lx.line[s..e];
            if YAML_KEYWORDS.contains(&word) {
                lx.fill(TokenKind::Keyword);
            } else if yaml_key_ahead(lx) {
                lx.fill(TokenKind::Function);
            } else {
                lx.skip();
            }
        } else if matches!(c, ':' | ',' | '{' | '}' | '[' | ']' | '?' | '@' | '`') {
            lx.bump(c.len_utf8());
            lx.fill(TokenKind::Operator);
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}

/// `'`…`'`: the escape is a doubled quote, not a backslash.
fn yaml_single_quoted(lx: &mut Lx) {
    lx.bump(1);
    loop {
        match lx.peek() {
            Some('\'') => {
                lx.bump(1);
                if lx.peek() == Some('\'') {
                    lx.bump(1); // escaped '' — keep scanning
                    continue;
                }
                break;
            }
            Some(c) => lx.bump(c.len_utf8()),
            None => break,
        }
    }
    lx.fill(TokenKind::String);
}

/// The word just eaten is a key when `:` follows it (`key:` or `key :`).
fn yaml_key_ahead(lx: &Lx) -> bool {
    let rest = lx.rest();
    if let Some(tail) = rest.strip_prefix(':') {
        return tail
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '{' | '[' | ','));
    }
    // `key : value` — allow blanks before the colon.
    let trimmed = rest.trim_start_matches([' ', '\t']);
    trimmed.starts_with(": ") || trimmed == ":" || trimmed.starts_with(":\t")
}

/// Bash: `#` comments at word starts, `'`/`"`/`` ` `` strings (all may span
/// lines), `$var` as types, the command word as a function, `-f` flags as
/// keywords (violet, matching the prompt highlighter).
pub(crate) fn lex_bash(lx: &mut Lx, block: &mut Option<Block>) {
    let mut expect_cmd = true;
    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if c == '#'
            && (lx.pos == 0
                || lx
                    .prev_byte()
                    .is_some_and(|b| matches!(b, b' ' | b'\t' | b';' | b'|' | b'&' | b'(')))
        {
            line_comment(lx);
        } else if c == '"' {
            quoted(lx, "\"", "\"", true, true, block);
            expect_cmd = false;
        } else if c == '\'' {
            quoted(lx, "'", "'", false, true, block);
            expect_cmd = false;
        } else if c == '`' {
            quoted(lx, "`", "`", true, true, block);
            expect_cmd = true; // `…` opens a fresh command position
        } else if c == '$' {
            if lx.rest().starts_with("$(") {
                // Command substitution opens a fresh command position.
                lx.bump(2);
                lx.fill(TokenKind::Operator);
            } else {
                bash_variable(lx);
                expect_cmd = false;
            }
        } else if matches!(c, '|' | ';' | '&' | '(' | ')' | '{' | '}' | '<' | '>' | '!') {
            lx.bump_while(|c| {
                matches!(c, '|' | ';' | '&' | '(' | ')' | '{' | '}' | '<' | '>' | '!')
            });
            lx.fill(TokenKind::Operator);
            expect_cmd = true;
        } else if c == '-'
            && lx.rest()[1..]
                .chars()
                .next()
                .is_some_and(|d| d.is_ascii_alphabetic())
        {
            lx.bump(1);
            lx.bump_while(|d| d.is_ascii_alphanumeric() || d == '=' || d == '.');
            lx.fill(TokenKind::Keyword);
            expect_cmd = false;
        } else if c.is_ascii_digit() {
            number(lx, false);
            expect_cmd = false;
        } else if is_ident_start(c) {
            let (s, e) = ident(lx, false);
            let word = &lx.line[s..e];
            if lx.starts("=") {
                // `NAME=value` — an assignment name; the command may follow.
                lx.fill(TokenKind::Type);
            } else if s > 0 && lx.line.as_bytes()[s - 1] == b'=' {
                // Glued to `=` — the assignment value, not a command.
                lx.skip();
            } else if BASH_KEYWORDS.contains(&word) {
                lx.fill(TokenKind::Keyword);
                // `then`/`do`/`function` & co. precede another command.
                expect_cmd = matches!(
                    word,
                    "if" | "elif"
                        | "else"
                        | "then"
                        | "while"
                        | "until"
                        | "do"
                        | "function"
                        | "time"
                );
            } else if expect_cmd {
                lx.fill(TokenKind::Function);
                expect_cmd = false;
            } else {
                lx.skip();
                expect_cmd = false;
            }
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}

/// `$x`, `${x}`, `$?`, `$$`, `$9` — variables render as types.
fn bash_variable(lx: &mut Lx) {
    lx.bump(1); // $
    match lx.peek() {
        Some('{') => {
            lx.bump(1);
            lx.eat_until("}", false);
        }
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {
            lx.bump_while(is_ident_cont);
        }
        Some(_) => lx.bump(1),
        None => {}
    }
    lx.fill(TokenKind::Type);
}

/// Markdown (inside ```markdown fences): `#` headings, `` ` `` code spans,
/// `>` quotes, list markers, `**…**`/`_…_` emphasis, `[text](url)` links.
pub(crate) fn lex_markdown(lx: &mut Lx, _block: &mut Option<Block>) {
    // Line-initial constructs may be preceded by whitespace.
    lx.bump_while(char::is_whitespace);
    lx.skip();
    if !lx.done() {
        // Heading: `#`…`######` + space.
        let hashes = lx.rest().bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && lx.rest()[hashes..].starts_with(' ') {
            lx.bump(hashes);
            lx.fill(TokenKind::Keyword);
            lx.pos = lx.line.len();
            lx.fill(TokenKind::Type);
            return;
        }
        // Thematic break: three or more of the same `-`, `*` or `_`.
        let t = lx.rest().trim_end();
        let uniform =
            |m: char| t.len() >= 3 && t.chars().all(|c| c == m || c == ' ') && t.contains(m);
        if uniform('-') || uniform('*') || uniform('_') {
            lx.pos = lx.line.len();
            lx.fill(TokenKind::Operator);
            return;
        }
        // Quote marker.
        if lx.starts(">") {
            lx.bump(1);
            lx.fill(TokenKind::Comment);
        }
        // List marker: `-`/`*`/`+` + space, or `1.`/`2)` + space.
        if matches!(lx.peek(), Some('-' | '*' | '+')) && lx.rest()[1..].starts_with(' ') {
            lx.bump(1);
            lx.fill(TokenKind::Operator);
        } else if lx.peek().is_some_and(|c| c.is_ascii_digit()) {
            let save = lx.pos;
            lx.bump_while(|c| c.is_ascii_digit());
            let is_marker = matches!(lx.peek(), Some('.' | ')'))
                && lx.line[(lx.pos + 1).min(lx.line.len())..].starts_with(' ');
            if is_marker {
                lx.bump(1);
                lx.fill(TokenKind::Operator);
            } else {
                lx.pos = save;
            }
        }
    }

    while !lx.done() {
        let c = match lx.peek() {
            Some(c) => c,
            None => break,
        };
        if c.is_whitespace() {
            lx.bump_while(char::is_whitespace);
            lx.skip();
        } else if c == '`' {
            // Code span; an unclosed one runs to EOL, a fence line is all operator.
            if at_line_start(lx) && lx.rest().starts_with("```") {
                lx.bump_while(|c| c == '`');
                lx.fill(TokenKind::Operator);
                lx.pos = lx.line.len();
                lx.fill(TokenKind::Type);
                return;
            }
            lx.bump(1);
            lx.eat_until("`", false);
            lx.fill(TokenKind::String);
        } else if lx.starts("**") || lx.starts("__") {
            let mark = lx.rest()[..2].to_string();
            lx.bump(2);
            lx.eat_until(&mark, false);
            lx.fill(TokenKind::Type);
        } else if c == '*' || c == '_' {
            let mark = c.to_string();
            lx.bump(1);
            lx.eat_until(&mark, false);
            lx.fill(TokenKind::Type);
        } else if c == '[' {
            lx.bump(1);
            lx.eat_until("]", false);
            lx.fill(TokenKind::Function);
            if lx.peek() == Some('(') {
                lx.bump(1);
                lx.eat_until(")", false);
                lx.fill(TokenKind::String);
            }
        } else {
            lx.bump(c.len_utf8());
            lx.skip();
        }
    }
}
