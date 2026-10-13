//! Shell lexer: words, quotes, escapes, expansions, and control operators.
//!
//! Tuned for risk classification. A word carries the text the shell sees after
//! quote removal, with every expansion kept verbatim (`$x`, `$(..)`, `` `..` ``)
//! so a computed word still looks computed. The source of every command
//! substitution, wherever it sits — unquoted, inside double quotes, inside
//! `${..}`, in an unquoted here-document — is handed back separately, because
//! the shell runs it. What the lexer cannot follow (an unterminated quote, a
//! `(` glued to a word) is reported, never guessed: the caller then refuses to
//! call the line safe.

/// One shell word or operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    /// Control and redirection operators (`&&`, `|`, `2>&1`, `<<`), which are
    /// never words of a command.
    pub is_operator: bool,
}

impl Token {
    pub fn word(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_operator: false,
        }
    }

    pub fn operator(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_operator: true,
        }
    }

    /// The command name without its directory (`/bin/rm` -> `rm`).
    pub fn basename(&self) -> String {
        self.text
            .rsplit('/')
            .next()
            .unwrap_or(&self.text)
            .to_string()
    }

    pub fn is_flag(&self) -> bool {
        !self.is_operator && self.text.starts_with('-') && self.text.len() > 1
    }

    /// Whether this flag requests recursion, including bundles like `-rf` or `-Rf`.
    pub fn is_recursive_flag(&self) -> bool {
        if !self.is_flag() {
            return false;
        }
        if self.text.starts_with("--") {
            return self.text == "--recursive";
        }
        self.text.contains('r') || self.text.contains('R')
    }
}

/// What the lexer could not resolve. Each one makes the whole line
/// unclassifiable.
pub(crate) const UNTERMINATED: &str = "an unterminated quote or substitution";
pub(crate) const GLUED_PAREN: &str =
    "a `(` glued to a word (an extended glob, a zsh glob qualifier or a fish substitution)";
pub(crate) const CASE_IN_SUBSTITUTION: &str = "a `case` statement inside a command substitution";

/// A lexed command line.
#[derive(Debug, Default)]
pub(crate) struct Lexed {
    pub(crate) tokens: Vec<Token>,
    /// Source of every command and process substitution in the line.
    pub(crate) nested: Vec<String>,
    pub(crate) unresolved: Vec<&'static str>,
}

/// Tokenize a shell command line, resolving quotes and escapes.
pub fn tokenize(command: &str) -> Vec<Token> {
    lex(command).tokens
}

/// The words of every simple command the line runs, substitutions included.
pub fn split_segments(command: &str) -> Vec<Vec<Token>> {
    crate::shell::parse(command)
        .commands
        .into_iter()
        .map(|command| command.words)
        .filter(|words| !words.is_empty())
        .collect()
}

pub(crate) fn lex(src: &str) -> Lexed {
    let mut lexer = Lexer {
        chars: src.chars().collect(),
        pos: 0,
        out: Lexed::default(),
        heredocs: Vec::new(),
    };
    lexer.run();
    lexer.out
}

mod scan;

struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    /// Unquoted delimiter: the body is expanded like a double-quoted string.
    expands: bool,
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    out: Lexed,
    heredocs: Vec<Heredoc>,
}

/// Characters that end a word.
fn ends_word(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | ')')
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    fn op(&mut self, text: &str, len: usize) {
        self.pos += len;
        self.out.tokens.push(Token::operator(text));
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let next = self.at(self.pos + 1);
            match c {
                ' ' | '\t' | '\r' => self.pos += 1,
                '\\' if next == Some('\n') => self.pos += 2,
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                '\n' => {
                    self.op("\n", 1);
                    self.read_heredoc_bodies();
                }
                ';' => match (next, self.at(self.pos + 2)) {
                    (Some(';'), Some('&')) => self.op(";;&", 3),
                    (Some(';'), _) => self.op(";;", 2),
                    (Some('&'), _) => self.op(";&", 2),
                    _ => self.op(";", 1),
                },
                '&' => match (next, self.at(self.pos + 2)) {
                    (Some('&'), _) => self.op("&&", 2),
                    (Some('>'), Some('>')) => self.op("&>>", 3),
                    (Some('>'), _) => self.op("&>", 2),
                    _ => self.op("&", 1),
                },
                '|' => match next {
                    Some('|') => self.op("||", 2),
                    Some('&') => self.op("|&", 2),
                    _ => self.op("|", 1),
                },
                '(' => {
                    if next == Some('(')
                        && let Some(end) = self.arithmetic_end(self.pos)
                    {
                        self.arithmetic_command(end);
                    } else {
                        self.op("(", 1);
                    }
                }
                ')' => self.op(")", 1),
                '<' | '>' if next == Some('(') => {
                    self.word();
                }
                '<' | '>' => self.redirect(String::new()),
                _ => {
                    self.word();
                }
            }
        }
    }

    /// `((expr))` as a command: arithmetic, whose only commands are the
    /// substitutions inside it. `end` is the index of the closing `)`.
    fn arithmetic_command(&mut self, end: usize) {
        let body: String = self.chars[self.pos + 2..end - 1].iter().collect();
        self.scan_expansions(&body);
        self.pos = end + 1;
        self.out.tokens.push(Token::word(format!("(({body}))")));
    }

    fn unterminated(&mut self) {
        self.out.unresolved.push(UNTERMINATED);
        self.pos = self.chars.len();
    }

    /// A redirection operator at `pos`, `fd` being the digits before it.
    fn redirect(&mut self, fd: String) {
        let c = self.peek();
        let next = self.at(self.pos + 1);
        let after = self.at(self.pos + 2);
        let dup_target = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || c == '-');
        if c == Some('>') {
            match next {
                Some('>') => self.op(&format!("{fd}>>"), 2),
                Some('|') => self.op(&format!("{fd}>|"), 2),
                Some('&') if dup_target(after) => {
                    let target = after.unwrap_or('-');
                    self.op(&format!("{fd}>&{target}"), 3);
                }
                Some('&') => self.op(&format!("{fd}>&"), 2),
                _ => self.op(&format!("{fd}>"), 1),
            }
            return;
        }
        match next {
            Some('<') if after == Some('<') => self.op(&format!("{fd}<<<"), 3),
            Some('<') => {
                let strip_tabs = after == Some('-');
                let op = if strip_tabs { "<<-" } else { "<<" };
                self.op(&format!("{fd}{op}"), if strip_tabs { 3 } else { 2 });
                self.heredoc_delimiter(strip_tabs);
            }
            Some('&') if dup_target(after) => {
                let target = after.unwrap_or('-');
                self.op(&format!("{fd}<&{target}"), 3);
            }
            Some('&') => self.op(&format!("{fd}<&"), 2),
            Some('>') => self.op(&format!("{fd}<>"), 2),
            _ => self.op(&format!("{fd}<"), 1),
        }
    }

    fn heredoc_delimiter(&mut self, strip_tabs: bool) {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.pos += 1;
        }
        if self
            .peek()
            .is_none_or(|c| ends_word(c) || c == '<' || c == '>')
        {
            return;
        }
        let quoted = self.word();
        if let Some(token) = self.out.tokens.last() {
            self.heredocs.push(Heredoc {
                delimiter: token.text.clone(),
                strip_tabs,
                expands: !quoted,
            });
        }
    }

    /// At the start of the line after a here-document operator: consume each
    /// pending body up to its delimiter line.
    fn read_heredoc_bodies(&mut self) {
        let heredocs = std::mem::take(&mut self.heredocs);
        let (bodies, end) = self.bodies(self.pos, &heredocs);
        self.pos = end;
        for (heredoc, body) in heredocs.iter().zip(bodies) {
            if heredoc.expands {
                self.scan_expansions(&body);
            }
        }
    }

    /// Read one word; returns whether any part of it was quoted or escaped.
    fn word(&mut self) -> bool {
        let mut text = String::new();
        let mut quoted = false;
        while let Some(c) = self.peek() {
            if ends_word(c) {
                break;
            }
            match c {
                '<' | '>' if self.at(self.pos + 1) == Some('(') => {
                    self.substitution(&mut text, c, self.pos + 1);
                }
                '<' | '>' => {
                    if !quoted && !text.is_empty() && text.chars().all(|d| d.is_ascii_digit()) {
                        self.redirect(text);
                        return quoted;
                    }
                    break;
                }
                '(' => {
                    if text.is_empty() || self.at(self.pos + 1) == Some(')') {
                        break;
                    }
                    self.out.unresolved.push(GLUED_PAREN);
                    self.substitution(&mut text, '(', self.pos);
                }
                '\'' => {
                    quoted = true;
                    let Some(end) = self.skip_until(self.pos + 1, '\'') else {
                        self.unterminated();
                        break;
                    };
                    text.extend(&self.chars[self.pos + 1..end]);
                    self.pos = end + 1;
                }
                '"' => {
                    quoted = true;
                    self.double_quoted(&mut text);
                }
                '\\' => {
                    quoted = true;
                    match self.at(self.pos + 1) {
                        // A line continuation joins the two lines.
                        Some('\n') => self.pos += 2,
                        Some(escaped) => {
                            text.push(escaped);
                            self.pos += 2;
                        }
                        None => self.pos += 1,
                    }
                }
                '$' => self.dollar(&mut text, false),
                '`' => self.backtick(&mut text),
                _ => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
        self.out.tokens.push(Token::word(text));
        quoted
    }

    fn double_quoted(&mut self, text: &mut String) {
        self.pos += 1;
        loop {
            match self.peek() {
                None => return self.unterminated(),
                Some('"') => {
                    self.pos += 1;
                    return;
                }
                Some('\\') => {
                    match self.at(self.pos + 1) {
                        Some('\n') => {}
                        Some(c @ ('"' | '\\' | '$' | '`')) => text.push(c),
                        Some(c) => {
                            text.push('\\');
                            text.push(c);
                        }
                        None => return self.unterminated(),
                    }
                    self.pos += 2;
                }
                Some('$') => self.dollar(text, true),
                Some('`') => self.backtick(text),
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    /// A `$` expansion at `pos`, appended to `text` verbatim.
    fn dollar(&mut self, text: &mut String, in_quotes: bool) {
        match self.at(self.pos + 1) {
            Some('(') => {
                if self.at(self.pos + 2) == Some('(')
                    && let Some(end) = self.arithmetic_end(self.pos + 1)
                {
                    let body: String = self.chars[self.pos + 3..end - 1].iter().collect();
                    self.scan_expansions(&body);
                    text.push_str(&format!("$(({body}))"));
                    self.pos = end + 1;
                } else {
                    self.substitution(text, '$', self.pos + 1);
                }
            }
            Some('{') => {
                let Some(end) = self.matching_brace(self.pos + 1) else {
                    return self.unterminated();
                };
                let body: String = self.chars[self.pos + 2..end].iter().collect();
                self.scan_expansions(&body);
                text.push_str(&format!("${{{body}}}"));
                self.pos = end + 1;
            }
            Some('\'') if !in_quotes => {
                let Some(end) = self.ansi_c_end(self.pos + 2) else {
                    return self.unterminated();
                };
                text.extend(&self.chars[self.pos..=end]);
                self.pos = end + 1;
            }
            _ => {
                text.push('$');
                self.pos += 1;
            }
        }
    }

    /// The closing quote of `$'..'`, where a backslash escapes the next char.
    fn ansi_c_end(&self, from: usize) -> Option<usize> {
        let mut index = from;
        while let Some(c) = self.at(index) {
            match c {
                '\\' => index += 1,
                '\'' => return Some(index),
                _ => {}
            }
            index += 1;
        }
        None
    }

    /// `$( .. )`, `<( .. )`, `>( .. )` or a glued `( .. )`: the parenthesised
    /// source is a nested command line. `open` is the index of the `(`.
    fn substitution(&mut self, text: &mut String, sigil: char, open: usize) {
        let Some(end) = self.matching_paren(open) else {
            return self.unterminated();
        };
        let body: String = self.chars[open + 1..end].iter().collect();
        if lex(&body)
            .tokens
            .iter()
            .any(|t| !t.is_operator && t.text == "case")
        {
            // A `case` pattern's `)` closes the substitution early in the
            // count above, so what follows it would be read as the outer line.
            self.out.unresolved.push(CASE_IN_SUBSTITUTION);
        }
        if sigil != '(' {
            text.push(sigil);
        }
        text.push('(');
        text.push_str(&body);
        text.push(')');
        self.out.nested.push(body);
        self.pos = end + 1;
    }

    /// `` `..` ``: a command substitution whose body unescapes `\``, `\\`, `\$`.
    fn backtick(&mut self, text: &mut String) {
        let mut body = String::new();
        let mut index = self.pos + 1;
        loop {
            match self.at(index) {
                None => return self.unterminated(),
                Some('`') => break,
                Some('\\') => {
                    match self.at(index + 1) {
                        Some(c @ ('`' | '\\' | '$')) => body.push(c),
                        Some(c) => {
                            body.push('\\');
                            body.push(c);
                        }
                        None => {}
                    }
                    index += 2;
                }
                Some(c) => {
                    body.push(c);
                    index += 1;
                }
            }
        }
        text.push('`');
        text.push_str(&body);
        text.push('`');
        self.out.nested.push(body);
        self.pos = index + 1;
    }
}

#[cfg(test)]
#[path = "tokenize_tests.rs"]
mod tokenize_tests;
