//! A line lexer: `lex_line` is called once per line and returns tokens that
//! partition the whole line, so a diff of a file fragment highlights without
//! seeing the file. The only state kept between lines is `block`: inside a
//! block comment, a raw/triple-quoted string, or a YAML block scalar.

use std::ops::Range;

use crate::syntax::code;
use crate::syntax::lang::Lang;
use crate::syntax::misc;

#[cfg(test)]
#[path = "lexer_tests.rs"]
mod lexer_tests;

/// Lines longer than this are never tokenized (minified blobs stay cheap).
pub const MAX_LINE_CHARS: usize = 2000;

/// The seven highlight kinds — each maps to an existing `Theme` colour via
/// [`crate::syntax::token_style`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword,
    Function,
    Type,
    String,
    Number,
    Comment,
    Operator,
}

/// A byte range of the line tagged with a kind. `kind: None` is plain code —
/// the tokens tile the line end to end, so `sum(range.len()) == line.len()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Option<TokenKind>,
    pub range: Range<usize>,
}

/// What an open block construct is waiting for on the next lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// Emit `kind` until `end` appears (the terminator is part of the token).
    /// `escape` makes `\x` skip `x`, so `\"` cannot close a `"` string.
    Until {
        end: String,
        kind: TokenKind,
        escape: bool,
    },
    /// A YAML `|`/`>` scalar: content while indented deeper than `indent`.
    YamlScalar { indent: usize },
}

/// Stateful per-line lexer. Build once per file/diff section, feed lines in
/// order; `Lexer::for_lang` at a fragment start is fine too.
pub struct Lexer {
    lang: Lang,
    block: Option<Block>,
}

impl Lexer {
    pub fn for_lang(lang: Lang) -> Self {
        Self { lang, block: None }
    }

    /// `detect_lang` + `for_lang`. `None` when the language is unknown.
    pub fn detect(path_or_info: &str) -> Option<Self> {
        crate::syntax::detect_lang(path_or_info).map(Self::for_lang)
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// True while inside a block comment / multiline string — a caller folding
    /// lines can keep the same lexer across the fold boundary harmlessly.
    pub fn in_block(&self) -> bool {
        self.block.is_some()
    }

    /// Tokenize one line. Tokens tile `[0, line.len())`; `kind` is `None` for
    /// unhighlighted text. Lines over `MAX_LINE_CHARS` come back as a single
    /// untagged token.
    pub fn lex_line(&mut self, line: &str) -> Vec<Token> {
        if line.len() > MAX_LINE_CHARS && line.chars().count() > MAX_LINE_CHARS {
            return vec![Token {
                kind: None,
                range: 0..line.len(),
            }];
        }
        let mut lx = Lx::new(line);
        if self.continue_block(&mut lx) {
            match self.lang {
                Lang::Rust => code::lex_rust(&mut lx, &mut self.block),
                Lang::Python => code::lex_python(&mut lx, &mut self.block),
                Lang::TypeScript => code::lex_tsjs(&mut lx, &mut self.block),
                Lang::Json => code::lex_json(&mut lx, &mut self.block),
                Lang::Toml => misc::lex_toml(&mut lx, &mut self.block),
                Lang::Yaml => misc::lex_yaml(&mut lx, &mut self.block),
                Lang::Bash => misc::lex_bash(&mut lx, &mut self.block),
                Lang::Markdown => misc::lex_markdown(&mut lx, &mut self.block),
            }
        }
        lx.finish()
    }

    /// Consume the leading part of the line while `self.block` is open.
    /// Returns false when a YAML scalar still owns the whole line.
    fn continue_block(&mut self, lx: &mut Lx) -> bool {
        match self.block.take() {
            None => true,
            Some(Block::Until { end, kind, escape }) => {
                let closed = lx.eat_until(end.as_str(), escape);
                lx.fill(kind);
                if !closed {
                    self.block = Some(Block::Until { end, kind, escape });
                }
                true
            }
            Some(Block::YamlScalar { indent }) => {
                let line = lx.line;
                let line_indent = misc::leading_indent(line);
                if line.trim().is_empty() || line_indent > indent {
                    lx.pos = line.len();
                    lx.fill(TokenKind::String);
                    self.block = Some(Block::YamlScalar { indent });
                    false
                } else {
                    true
                }
            }
        }
    }
}

/// Byte cursor over one line plus the token sink.
pub(crate) struct Lx<'a> {
    pub line: &'a str,
    pub pos: usize,
    out: Vec<Token>,
    mark: usize,
}

impl<'a> Lx<'a> {
    pub fn new(line: &'a str) -> Self {
        Self {
            line,
            pos: 0,
            out: Vec::new(),
            mark: 0,
        }
    }

    pub fn rest(&self) -> &'a str {
        &self.line[self.pos..]
    }

    pub fn done(&self) -> bool {
        self.pos >= self.line.len()
    }

    pub fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    pub fn starts(&self, pat: &str) -> bool {
        self.rest().starts_with(pat)
    }

    /// The byte before `pos`, if any — comment rules like bash `#` need it.
    pub fn prev_byte(&self) -> Option<u8> {
        self.pos.checked_sub(1).map(|i| self.line.as_bytes()[i])
    }

    /// Advance `n` bytes without emitting.
    pub fn bump(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.line.len());
    }

    /// Advance while `pred` holds; returns the number of bytes consumed.
    pub fn bump_while(&mut self, pred: impl Fn(char) -> bool) -> usize {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if !pred(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        self.pos - start
    }

    /// Advance until `pat` appears; on success the terminator is consumed too
    /// and the return is true. `escape` skips the char after a backslash.
    pub fn eat_until(&mut self, pat: &str, escape: bool) -> bool {
        while !self.done() {
            if self.starts(pat) {
                self.bump(pat.len());
                return true;
            }
            if escape && self.peek() == Some('\\') {
                self.bump(1);
                if let Some(c) = self.peek() {
                    self.bump(c.len_utf8());
                }
                continue;
            }
            let c = self.peek();
            match c {
                Some(c) => self.bump(c.len_utf8()),
                None => break,
            }
        }
        false
    }

    /// Tag everything consumed since the last `fill`/`emit` with `kind`.
    pub fn fill(&mut self, kind: TokenKind) {
        self.emit(Some(kind));
    }

    /// Tag consumed text as unhighlighted. Equivalent to never filling —
    /// `finish` tags leftovers `None` — but merging keeps tokens few.
    pub fn skip(&mut self) {
        self.emit(None);
    }

    fn emit(&mut self, kind: Option<TokenKind>) {
        if self.pos <= self.mark {
            return;
        }
        let range = self.mark..self.pos;
        if let Some(last) = self.out.last_mut()
            && last.kind == kind
        {
            last.range.end = self.pos;
        } else {
            self.out.push(Token { kind, range });
        }
        self.mark = self.pos;
    }

    /// Fill trailing unmarked bytes and return the tokens.
    pub fn finish(mut self) -> Vec<Token> {
        self.emit(None);
        self.out
    }
}

// --- Shared bites -------------------------------------------------------

pub(crate) fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

pub(crate) fn is_ident_cont(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `//` / `#` to the end of the line.
pub(crate) fn line_comment(lx: &mut Lx) {
    lx.pos = lx.line.len();
    lx.fill(TokenKind::Comment);
}

/// `/* … */`; an unterminated block sets `block` so the next line continues.
pub(crate) fn block_comment(lx: &mut Lx, open: &str, close: &str, block: &mut Option<Block>) {
    lx.bump(open.len());
    if lx.eat_until(close, false) {
        lx.fill(TokenKind::Comment);
    } else {
        lx.fill(TokenKind::Comment);
        *block = Some(Block::Until {
            end: close.to_string(),
            kind: TokenKind::Comment,
            escape: false,
        });
    }
}

/// A quoted string. `close` is usually the same as `open`; `multiline` strings
/// leave `block` set when the line runs out before the terminator.
pub(crate) fn quoted(
    lx: &mut Lx,
    open: &str,
    close: &str,
    escape: bool,
    multiline: bool,
    block: &mut Option<Block>,
) {
    lx.bump(open.len());
    if lx.eat_until(close, escape) {
        lx.fill(TokenKind::String);
    } else {
        lx.fill(TokenKind::String);
        if multiline {
            *block = Some(Block::Until {
                end: close.to_string(),
                kind: TokenKind::String,
                escape,
            });
        }
    }
}

/// A number run: digits plus the characters that complete literals. Code mode
/// (`loose: false`) joins `0x..`, `1_000`, `1.5`, `0..10`, and a `+`/`-` only
/// right after an exponent letter (`1e-3`). Loose mode (TOML/YAML/JSON) also
/// joins `-`, `:` and `+`, so dates like `1979-05-27T07:32:00Z` stay one token.
/// A leading `+`/`-` counts only when a digit follows.
pub(crate) fn number(lx: &mut Lx, loose: bool) {
    if matches!(lx.peek(), Some('-' | '+'))
        && lx.rest().chars().nth(1).is_some_and(|c| c.is_ascii_digit())
    {
        lx.bump(1);
    }
    let mut prev = '\0';
    while let Some(c) = lx.peek() {
        let ok = if loose {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-' | '+')
        } else {
            c.is_ascii_alphanumeric()
                || c == '_'
                || c == '.'
                || (matches!(c, '+' | '-') && matches!(prev, 'e' | 'E' | 'p' | 'P'))
        };
        if !ok {
            break;
        }
        prev = c;
        lx.pos += c.len_utf8();
    }
    lx.fill(TokenKind::Number);
}

/// An identifier/keyword run; `dash` lets `-` join (`toml-key`, `yaml-key`)
/// only between two identifier characters.
pub(crate) fn ident(lx: &mut Lx, dash: bool) -> (usize, usize) {
    let start = lx.pos;
    while let Some(c) = lx.peek() {
        if is_ident_cont(c) {
            lx.pos += c.len_utf8();
        } else if dash
            && c == '-'
            && lx.rest().chars().nth(1).is_some_and(is_ident_cont)
            && lx.pos > start
        {
            lx.pos += 1;
        } else {
            break;
        }
    }
    (start, lx.pos)
}
