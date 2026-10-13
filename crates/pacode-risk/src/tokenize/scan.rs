//! Reading ahead without emitting tokens: where a substitution, a `${..}` or
//! a quoted string ends, what a here-document's delimiter and body are, and
//! which substitutions sit in text the shell expands like a double-quoted
//! string.

use super::{Heredoc, Lexed, Lexer, ends_word};

/// Whether a word starts after `c`, so a `#` there opens a comment.
fn starts_word_after(c: char) -> bool {
    ends_word(c) || c == '('
}

impl Lexer {
    /// For `((` at `open`: the index of the `)` closing it, when the two
    /// parentheses close together (`))`), which is what makes it arithmetic
    /// rather than a subshell in a subshell.
    pub(super) fn arithmetic_end(&self, open: usize) -> Option<usize> {
        let outer = self.matching_paren(open)?;
        let inner = self.matching_paren(open + 1)?;
        (outer == inner + 1).then_some(outer)
    }

    /// The index of the `)` matching the `(` at `open`, skipping quoted text,
    /// comments and here-document bodies, as the shell does when it reads a
    /// substitution (an apostrophe in a commit message must not open a quote).
    pub(super) fn matching_paren(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut index = open;
        let mut heredocs = Vec::new();
        while let Some(c) = self.at(index) {
            match c {
                '\\' => index += 1,
                '\'' => index = self.skip_until(index + 1, '\'')?,
                '"' => index = self.skip_double_quoted(index + 1)?,
                '`' => index = self.skip_until(index + 1, '`')?,
                '#' if index > open && self.at(index - 1).is_some_and(starts_word_after) => {
                    while self.at(index + 1).is_some_and(|c| c != '\n') {
                        index += 1;
                    }
                }
                '<' if self.at(index + 1) == Some('<') => {
                    if self.at(index + 2) == Some('<') {
                        index += 2;
                    } else {
                        let (heredoc, end) = self.heredoc_operand(index + 2);
                        heredocs.extend(heredoc);
                        index = end;
                    }
                }
                '\n' if !heredocs.is_empty() => {
                    index = self.bodies(index + 1, &std::mem::take(&mut heredocs)).1 - 1;
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    /// The index of the `}` matching the `{` at `open`, skipping quoted text.
    pub(super) fn matching_brace(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut index = open;
        while let Some(c) = self.at(index) {
            match c {
                '\\' => index += 1,
                '\'' => index = self.skip_until(index + 1, '\'')?,
                '"' => index = self.skip_double_quoted(index + 1)?,
                '`' => index = self.skip_until(index + 1, '`')?,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    /// Index of the next unescaped `close` from `from`.
    pub(super) fn skip_until(&self, from: usize, close: char) -> Option<usize> {
        let mut index = from;
        while let Some(c) = self.at(index) {
            if c == '\\' && close != '\'' {
                index += 2;
                continue;
            }
            if c == close {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    pub(super) fn skip_double_quoted(&self, from: usize) -> Option<usize> {
        let mut index = from;
        while let Some(c) = self.at(index) {
            match c {
                '\\' => index += 1,
                '"' => return Some(index),
                '$' if self.at(index + 1) == Some('(') => index = self.matching_paren(index + 1)?,
                '`' => index = self.skip_until(index + 1, '`')?,
                _ => {}
            }
            index += 1;
        }
        None
    }

    /// The bodies of `heredocs`, read in order from `from` (the line after
    /// their operators), and the index after the last delimiter line.
    pub(super) fn bodies(&self, from: usize, heredocs: &[Heredoc]) -> (Vec<String>, usize) {
        let mut pos = from;
        let mut bodies = Vec::with_capacity(heredocs.len());
        for heredoc in heredocs {
            let mut body = String::new();
            while pos < self.chars.len() {
                let start = pos;
                while self.at(pos).is_some_and(|c| c != '\n') {
                    pos += 1;
                }
                let line: String = self.chars[start..pos].iter().collect();
                pos = (pos + 1).min(self.chars.len());
                let line = line.trim_end_matches('\r');
                let candidate = if heredoc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                if candidate == heredoc.delimiter {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            bodies.push(body);
        }
        (bodies, pos)
    }

    /// The delimiter after a `<<` at `from - 2`, read without emitting
    /// tokens, and the index of its last character.
    pub(super) fn heredoc_operand(&self, from: usize) -> (Option<Heredoc>, usize) {
        let mut index = from;
        let strip_tabs = self.at(index) == Some('-');
        if strip_tabs {
            index += 1;
        }
        while self.at(index).is_some_and(|c| c == ' ' || c == '\t') {
            index += 1;
        }
        let mut delimiter = String::new();
        let mut quoted = false;
        while let Some(c) = self.at(index) {
            match c {
                '\'' | '"' => {
                    quoted = true;
                    let end = if c == '\'' {
                        self.skip_until(index + 1, '\'')
                    } else {
                        self.skip_double_quoted(index + 1)
                    };
                    let Some(end) = end else {
                        break;
                    };
                    delimiter.extend(&self.chars[index + 1..end]);
                    index = end + 1;
                }
                '\\' => {
                    quoted = true;
                    delimiter.extend(self.at(index + 1));
                    index += 2;
                }
                c if c.is_whitespace() || ";&|<>()".contains(c) => break,
                c => {
                    delimiter.push(c);
                    index += 1;
                }
            }
        }
        let heredoc = (!delimiter.is_empty()).then_some(Heredoc {
            delimiter,
            strip_tabs,
            expands: !quoted,
        });
        (heredoc, index - 1)
    }

    /// Record the substitutions in text the shell expands like a
    /// double-quoted string: `${..}` bodies, arithmetic, here-documents.
    /// Quotes there are literal, so every `$(`, `$((` and backtick counts.
    pub(super) fn scan_expansions(&mut self, text: &str) {
        let mut sub = Lexer {
            chars: text.chars().collect(),
            pos: 0,
            out: Lexed::default(),
            heredocs: Vec::new(),
        };
        let mut scratch = String::new();
        while let Some(c) = sub.peek() {
            match c {
                '\\' => sub.pos += 2,
                '$' => sub.dollar(&mut scratch, true),
                '`' => sub.backtick(&mut scratch),
                _ => sub.pos += 1,
            }
        }
        self.out.nested.append(&mut sub.out.nested);
        self.out.unresolved.append(&mut sub.out.unresolved);
    }
}
