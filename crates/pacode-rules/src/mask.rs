//! Best-effort code masking: blank out comments and string/char literals so that
//! `scope: code` rules do not fire on prose. The output has exactly the same byte
//! length and the same newlines as the input (masked bytes become spaces), so line
//! numbers and byte offsets carry over unchanged.
//!
//! Not a real lexer: JS regex literals, JSX text and exotic raw strings can
//! confuse it. A confused mask only ever causes a missed or an extra hit on that
//! line, never a panic.

/// Comment/string family of a language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    /// `//`, nested `/* */`, `"..."`, raw strings, char literals vs lifetimes.
    Rust,
    /// `//`, `/* */`, `"..."`, `'...'`, `` `...` `` (TS, JS, Java, Go, C, ...).
    CLike,
    /// `#` comments, `"..."`, `'...'`, triple quotes (Python, shell, TOML, YAML).
    Hash,
}

impl Syntax {
    /// Syntax for a lowercase file extension; `None` when unknown (no masking).
    pub fn for_ext(ext: &str) -> Option<Syntax> {
        match ext {
            "rs" => Some(Syntax::Rust),
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" | "java" | "go" | "c"
            | "h" | "cc" | "cpp" | "hpp" | "cs" | "kt" | "swift" | "scala" | "dart" => {
                Some(Syntax::CLike)
            }
            "py" | "sh" | "bash" | "zsh" | "rb" | "toml" | "yaml" | "yml" => Some(Syntax::Hash),
            _ => None,
        }
    }
}

fn blank(out: &mut [u8], from: usize, to: usize) {
    let end = to.min(out.len());
    for b in out.iter_mut().take(end).skip(from) {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// End (exclusive) of a line comment starting at `i`: the next `\n` or EOF.
fn line_end(bytes: &[u8], i: usize) -> usize {
    bytes[i..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| i + p)
}

/// End (exclusive) of a block comment starting at `i` (`/*`).
fn block_end(bytes: &[u8], i: usize, nested: bool) -> usize {
    let mut depth = 1usize;
    let mut j = i + 2;
    while j < bytes.len() {
        if bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return j;
            }
        } else if nested && bytes[j] == b'/' && bytes.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else {
            j += 1;
        }
    }
    bytes.len()
}

/// Index of the closing quote of a string whose opening quote is at `i`;
/// `bytes.len()` when unterminated. `multiline` lets it span newlines.
fn string_end(bytes: &[u8], i: usize, multiline: bool) -> usize {
    let quote = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'\n' if !multiline => return j,
            b if b == quote => return j,
            _ => j += 1,
        }
    }
    bytes.len()
}

/// Rust raw string at `i` (`r"`, `r#"`, `br##"`): returns (content_start, close_start, end).
fn rust_raw_string(bytes: &[u8], i: usize) -> Option<(usize, usize, usize)> {
    if i > 0 && is_ident(bytes[i - 1]) {
        return None;
    }
    let mut j = i;
    if bytes.get(j) == Some(&b'b') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let hashes = bytes[j..].iter().take_while(|&&b| b == b'#').count();
    j += hashes;
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    let content = j + 1;
    let mut k = content;
    while k < bytes.len() {
        if bytes[k] == b'"' && bytes[k + 1..].iter().take_while(|&&b| b == b'#').count() >= hashes {
            return Some((content, k, k + 1 + hashes));
        }
        k += 1;
    }
    Some((content, bytes.len(), bytes.len()))
}

/// Rust char literal at `i` (`'x'`, `'\n'`): returns the index of the closing quote.
fn rust_char_end(text: &str, i: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(i + 1) == Some(&b'\\') {
        let limit = (i + 12).min(bytes.len());
        return (i + 3..limit).find(|&j| bytes[j] == b'\'');
    }
    let c = text.get(i + 1..)?.chars().next()?;
    let close = i + 1 + c.len_utf8();
    (bytes.get(close) == Some(&b'\'') && c != '\'').then_some(close)
}

/// Blank comments and literal contents of `text` for the given syntax.
pub fn mask_code(text: &str, syntax: Syntax) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        match syntax {
            Syntax::Rust | Syntax::CLike if b == b'/' && next == Some(b'/') => {
                let end = line_end(bytes, i);
                blank(&mut out, i, end);
                i = end;
            }
            Syntax::Rust | Syntax::CLike if b == b'/' && next == Some(b'*') => {
                let end = block_end(bytes, i, syntax == Syntax::Rust);
                blank(&mut out, i, end);
                i = end;
            }
            Syntax::Hash if b == b'#' => {
                let end = line_end(bytes, i);
                blank(&mut out, i, end);
                i = end;
            }
            Syntax::Rust if (b == b'r' || b == b'b') && rust_raw_string(bytes, i).is_some() => {
                if let Some((start, close, end)) = rust_raw_string(bytes, i) {
                    blank(&mut out, start, close);
                    i = end.max(i + 1);
                }
            }
            Syntax::Rust if b == b'\'' => match rust_char_end(text, i) {
                Some(close) => {
                    blank(&mut out, i + 1, close);
                    i = close + 1;
                }
                None => i += 1,
            },
            Syntax::Hash if (b == b'"' || b == b'\'') && bytes[i..].starts_with(&[b, b, b]) => {
                let close = (i + 3..bytes.len().saturating_sub(2))
                    .find(|&j| bytes[j..].starts_with(&[b, b, b]))
                    .unwrap_or(bytes.len());
                blank(&mut out, i + 3, close);
                i = (close + 3).min(bytes.len());
            }
            Syntax::Rust if b == b'"' => {
                let end = string_end(bytes, i, true);
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            Syntax::CLike | Syntax::Hash if b == b'"' || b == b'\'' => {
                let end = string_end(bytes, i, false);
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            Syntax::CLike if b == b'`' => {
                let end = string_end(bytes, i, true);
                blank(&mut out, i + 1, end);
                i = end + 1;
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Per-line flags (index = 0-based line) marking Rust test code: items annotated
/// `#[cfg(test)]`, `#[test]` or `#[tokio::test]`, including their whole `{ ... }`
/// body. Input should be the masked text so braces in strings do not count.
pub fn rust_test_lines(masked: &str) -> Vec<bool> {
    const MAX_LOOKAHEAD: usize = 24;
    let lines: Vec<&str> = masked.lines().collect();
    let mut flags = vec![false; lines.len()];
    let mut region_depth: Option<i64> = None;
    let mut pending: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        if let Some(depth) = region_depth.as_mut() {
            flags[idx] = true;
            *depth += brace_delta(line);
            if *depth <= 0 {
                region_depth = None;
            }
            continue;
        }
        let mut rest: &str = line;
        if let Some(attr_end) = test_attr_end(line) {
            pending.get_or_insert(idx);
            rest = &line[attr_end..];
        }
        let Some(start) = pending else { continue };
        match rest.find(['{', ';']) {
            Some(pos) if rest.as_bytes()[pos] == b';' => {
                flags[start..=idx].iter_mut().for_each(|f| *f = true);
                pending = None;
            }
            Some(pos) => {
                flags[start..=idx].iter_mut().for_each(|f| *f = true);
                let depth = brace_delta(&rest[pos..]);
                if depth > 0 {
                    region_depth = Some(depth);
                }
                pending = None;
            }
            None if idx - start >= MAX_LOOKAHEAD => pending = None,
            None => {}
        }
    }
    flags
}

fn brace_delta(line: &str) -> i64 {
    line.bytes().fold(0, |acc, b| match b {
        b'{' => acc + 1,
        b'}' => acc - 1,
        _ => acc,
    })
}

/// Byte offset just past a test attribute on this line, if present.
fn test_attr_end(line: &str) -> Option<usize> {
    ["#[cfg(test)]", "#[test]", "#[tokio::test"]
        .iter()
        .find_map(|needle| {
            let pos = line.find(needle)?;
            let after = pos + needle.len();
            if needle.ends_with("test") {
                let close = line[after..].find(']')?;
                return Some(after + close + 1);
            }
            Some(after)
        })
}
