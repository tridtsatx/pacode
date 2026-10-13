//! Shell structure: the simple commands a command line runs.
//!
//! The risk gate and the read-only check ask the same question of a line —
//! which programs run, with which words and redirections — so the grammar
//! around a command lives here, once: separators (`;`, `&`, `&&`, `||`, newline,
//! `|`, `|&`, parentheses), words that precede a command without being one
//! (`if`, `!`, `{`, `then`, fish's `and`, zsh's `noglob`, ...), assignments and
//! redirections in front of it, `case` patterns, `for` word lists, function
//! headers, `[[ ]]` and `(( ))`. Substitutions are command lines of their own;
//! their commands join the list.
//!
//! The command runs through `$SHELL -c`, which may be bash, dash, zsh or fish,
//! so where they read a line differently the parser keeps every reading that
//! runs more: dash takes `ls &>f rm x` as `ls &` then `>f rm x`, and the
//! command after the redirection is listed too.

use crate::tokenize::{self, Token};

/// Deeper substitutions than this are not followed.
const MAX_NESTING: usize = 8;
const TOO_DEEP: &str = "substitutions nested too deeply";
const UNCLOSED: &str = "an unclosed `case` or `[[`";

/// Words in command position that are grammar, not a program: the command
/// follows them.
const COMMAND_PREFIXES: &[&str] = &[
    "{",
    "}",
    "!",
    "if",
    "then",
    "else",
    "elif",
    "fi",
    "do",
    "done",
    "while",
    "until",
    "in",
    // fish
    "and",
    "or",
    "not",
    "begin",
    "end", // zsh precommand modifiers
    "noglob",
    "nocorrect",
    "-",
];

/// One simple command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimpleCommand {
    /// `NAME=value` words before the program.
    pub assignments: Vec<Token>,
    /// The program and its arguments; empty for a line of only redirections
    /// or assignments.
    pub words: Vec<Token>,
    pub redirects: Vec<Redirect>,
    /// Its standard input comes from a pipe.
    pub receives_pipe: bool,
}

/// A redirection and the file it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub op: String,
    /// `None` for a descriptor copy (`2>&1`, `<&-`). For a here-document it is
    /// the delimiter word.
    pub target: Option<Token>,
    /// The here-document body, for `<<` / `<<-`, so a shell reading its program
    /// from the heredoc can be assessed like a `-c` payload.
    pub heredoc_body: Option<String>,
}

impl Redirect {
    fn bare_op(&self) -> &str {
        self.op.trim_start_matches(|c: char| c.is_ascii_digit())
    }

    /// The leading file descriptor, empty when the default is used.
    fn fd(&self) -> &str {
        let digits = self.op.len() - self.bare_op().len();
        &self.op[..digits]
    }

    /// Opens its target for writing (`>`, `>>`, `>|`, `&>`, `&>>`, `>&f`, `<>`).
    pub fn writes(&self) -> bool {
        self.target.is_some() && self.bare_op().contains('>')
    }

    /// Truncates its target on open.
    pub fn truncates(&self) -> bool {
        self.target.is_some() && matches!(self.bare_op(), ">" | ">|" | "&>" | ">&")
    }

    /// Feeds the command's standard input from this redirection: `<`, `<<`,
    /// `<<-`, `<<<` or `<>` on fd 0 (the default). A shell whose stdin comes
    /// this way runs what it reads.
    pub fn reads_stdin(&self) -> bool {
        matches!(self.fd(), "" | "0") && matches!(self.bare_op(), "<" | "<<" | "<<-" | "<<<" | "<>")
    }

    /// A here-document (`<<`, `<<-`): its body is in [`Self::heredoc_body`].
    pub fn is_heredoc(&self) -> bool {
        matches!(self.bare_op(), "<<" | "<<-")
    }

    /// A here-string (`<<<`): its body is the target word.
    pub fn is_here_string(&self) -> bool {
        self.bare_op() == "<<<"
    }
}

/// Every simple command of a line, substitutions included.
#[derive(Debug, Default)]
pub struct Script {
    pub commands: Vec<SimpleCommand>,
    /// Command and process substitutions, at any depth.
    pub substitutions: usize,
    /// Why the line cannot be read statically; empty when it can.
    pub unresolved: Vec<&'static str>,
}

pub fn parse(src: &str) -> Script {
    let mut script = Script::default();
    parse_into(src, 0, &mut script);
    script
}

fn parse_into(src: &str, depth: usize, script: &mut Script) {
    if depth > MAX_NESTING {
        script.unresolved.push(TOO_DEEP);
        return;
    }
    let lexed = tokenize::lex(src);
    script.unresolved.extend(lexed.unresolved);
    Parser {
        tokens: &lexed.tokens,
        index: 0,
        script,
        current: SimpleCommand::default(),
        dash_split: None,
        cases: Vec::new(),
        heredoc_bodies: &lexed.heredoc_bodies,
        heredoc_index: 0,
    }
    .run();
    for nested in &lexed.nested {
        script.substitutions += 1;
        parse_into(nested, depth + 1, script);
    }
}

/// `Some(takes_target)` for a redirection operator.
fn redirection(op: &str) -> Option<bool> {
    let bare = op.trim_start_matches(|c: char| c.is_ascii_digit());
    if matches!(bare, "&>" | "&>>") {
        return Some(true);
    }
    if !bare.starts_with(['<', '>']) {
        return None;
    }
    let copies_descriptor = (bare.starts_with(">&") || bare.starts_with("<&")) && bare.len() > 2;
    Some(!copies_descriptor)
}

/// `NAME=value`, `NAME+=value`, `NAME[i]=value`.
pub(crate) fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let name = name.strip_suffix('+').unwrap_or(name);
    let name = match name.split_once('[') {
        Some((base, subscript)) if subscript.ends_with(']') => base,
        Some(_) => return false,
        None => name,
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Patterns,
    Body,
}

struct Parser<'a> {
    tokens: &'a [Token],
    index: usize,
    script: &'a mut Script,
    current: SimpleCommand,
    /// Where dash's reading of `&>` starts a command in `current.words`.
    dash_split: Option<usize>,
    cases: Vec<Case>,
    /// Here-document bodies in source order; the next `<<` / `<<-` takes one.
    heredoc_bodies: &'a [String],
    heredoc_index: usize,
}

impl Parser<'_> {
    fn peek(&self, ahead: usize) -> Option<&Token> {
        self.tokens.get(self.index + ahead)
    }

    fn peek_word(&self, ahead: usize) -> Option<&str> {
        self.peek(ahead)
            .filter(|t| !t.is_operator)
            .map(|t| t.text.as_str())
    }

    fn run(mut self) {
        while let Some(token) = self.peek(0).cloned() {
            if token.is_operator {
                self.operator(&token.text);
            } else {
                self.word(token);
            }
        }
        self.finish();
        if !self.cases.is_empty() {
            self.script.unresolved.push(UNCLOSED);
        }
    }

    fn operator(&mut self, op: &str) {
        self.index += 1;
        if let Some(takes_target) = redirection(op) {
            let target = match self.peek(0) {
                Some(t) if takes_target && !t.is_operator => {
                    let target = t.clone();
                    self.index += 1;
                    Some(target)
                }
                _ => None,
            };
            if matches!(op, "&>" | "&>>") && self.dash_split.is_none() {
                self.dash_split = Some(self.current.words.len());
            }
            let bare = op.trim_start_matches(|c: char| c.is_ascii_digit());
            let heredoc_body = if matches!(bare, "<<" | "<<-") {
                let body = self.heredoc_bodies.get(self.heredoc_index).cloned();
                self.heredoc_index += 1;
                body
            } else {
                None
            };
            self.current.redirects.push(Redirect {
                op: op.to_string(),
                target,
                heredoc_body,
            });
            return;
        }
        self.finish();
        self.current.receives_pipe = matches!(op, "|" | "|&");
        if let Some(case) = self.cases.last_mut() {
            match (op, *case) {
                (";;" | ";&" | ";;&", _) => *case = Case::Patterns,
                (")", Case::Patterns) => {
                    *case = Case::Body;
                    self.current.receives_pipe = false;
                }
                _ => {}
            }
        }
    }

    fn word(&mut self, token: Token) {
        self.index += 1;
        if self.cases.last() == Some(&Case::Patterns) {
            if token.text == "esac" {
                self.cases.pop();
            }
            return;
        }
        if !self.current.words.is_empty() {
            self.current.words.push(token);
            return;
        }
        let text = token.text.as_str();
        if is_assignment(text) {
            self.current.assignments.push(token);
            return;
        }
        if text.starts_with("((") && text.ends_with("))") {
            // Arithmetic: its substitutions were taken out by the lexer.
            return;
        }
        if COMMAND_PREFIXES.contains(&text) {
            return;
        }
        match text {
            "esac" => {
                self.cases.pop();
            }
            "coproc" => {
                // `coproc NAME { ..; }` / `coproc NAME ( .. )`: NAME is no command.
                let compound = self
                    .peek(1)
                    .is_some_and(|t| t.text == "{" || (t.is_operator && t.text == "("));
                if compound && self.peek_word(0).is_some() {
                    self.index += 1;
                }
            }
            "function" => {
                if self.peek_word(0).is_some() {
                    self.index += 1;
                }
            }
            "repeat" => self.index += 1,
            "for" | "select" | "foreach" => self.loop_header(),
            "case" => self.case_header(),
            "[[" => self.test_expression(),
            _ => {
                let defines_function = self.peek(0).is_some_and(|t| t.is_operator && t.text == "(")
                    && self.peek(1).is_some_and(|t| t.is_operator && t.text == ")");
                if defines_function {
                    self.index += 2;
                } else {
                    self.current.words.push(token);
                }
            }
        }
    }

    /// `for NAME in WORDS` / `for NAME do` / `for ((..))`: the name and the
    /// word list are not commands; `do` (or fish's next line) starts the body.
    fn loop_header(&mut self) {
        if self.peek_word(0).is_some() {
            self.index += 1;
        }
        if self.peek_word(0) == Some("in") {
            self.index += 1;
            while self.peek_word(0).is_some() {
                self.index += 1;
            }
        }
    }

    /// bash's `case WORD in`, then patterns; fish's `case PATTERN..` line.
    fn case_header(&mut self) {
        if self.peek_word(0).is_some() && self.peek_word(1) == Some("in") {
            self.index += 2;
            self.cases.push(Case::Patterns);
            return;
        }
        while self.peek_word(0).is_some() {
            self.index += 1;
        }
    }

    /// `[[ .. ]]`: a test whose `<`, `>`, `&&`, `(` are its own operators.
    fn test_expression(&mut self) {
        loop {
            match self.peek(0) {
                Some(t) if !t.is_operator && t.text == "]]" => {
                    self.index += 1;
                    return;
                }
                Some(t) if t.is_operator && matches!(t.text.as_str(), ";" | "\n" | "&" | "|") => {
                    break;
                }
                Some(_) => self.index += 1,
                None => break,
            }
        }
        self.script.unresolved.push(UNCLOSED);
    }

    fn finish(&mut self) {
        let command = std::mem::take(&mut self.current);
        if let Some(split) = self.dash_split.take()
            && split < command.words.len()
        {
            let mut words = command.words[split..].to_vec();
            let assignments = words.iter().take_while(|w| is_assignment(&w.text)).count();
            let rest = words.split_off(assignments);
            self.script.commands.push(SimpleCommand {
                assignments: words,
                words: rest,
                redirects: Vec::new(),
                receives_pipe: false,
            });
        }
        if !command.words.is_empty()
            || !command.redirects.is_empty()
            || !command.assignments.is_empty()
        {
            self.script.commands.push(command);
        }
    }
}

/// Brace expansion of one word: `{a,b}` and `x{1..3}`. `None` when it would
/// produce more than `cap` words.
pub(crate) fn expand_braces(word: &str, cap: usize) -> Option<Vec<String>> {
    let chars: Vec<char> = word.chars().collect();
    let Some((open, close, parts)) = first_brace_group(&chars, cap) else {
        return Some(vec![word.to_string()]);
    };
    let prefix: String = chars[..open].iter().collect();
    let suffix: String = chars[close + 1..].iter().collect();
    let mut out = Vec::new();
    for part in parts {
        for expanded in expand_braces(&format!("{prefix}{part}{suffix}"), cap)? {
            out.push(expanded);
            if out.len() > cap {
                return None;
            }
        }
    }
    Some(out)
}

/// The first `{..}` that brace-expands: a top-level comma list or an
/// `a..b` range. `${..}` is a parameter expansion and is skipped.
fn first_brace_group(chars: &[char], cap: usize) -> Option<(usize, usize, Vec<String>)> {
    let mut start = 0;
    while let Some(offset) = chars[start..].iter().position(|&c| c == '{') {
        let open = start + offset;
        start = open + 1;
        if open > 0 && chars[open - 1] == '$' {
            continue;
        }
        let mut depth = 0usize;
        let mut commas = Vec::new();
        let mut close = None;
        for (index, &c) in chars.iter().enumerate().skip(open) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(index);
                        break;
                    }
                }
                ',' if depth == 1 => commas.push(index),
                _ => {}
            }
        }
        let Some(close) = close else {
            continue;
        };
        let body: String = chars[open + 1..close].iter().collect();
        if !commas.is_empty() {
            let mut parts = Vec::new();
            let mut from = open + 1;
            for comma in commas.into_iter().chain([close]) {
                parts.push(chars[from..comma].iter().collect());
                from = comma + 1;
            }
            return Some((open, close, parts));
        }
        if let Some(parts) = range(&body, cap) {
            return Some((open, close, parts));
        }
    }
    None
}

/// `1..3` or `a..c`. A longer range than `cap` stops at `cap + 1` words, which
/// is enough for the caller to see it overflow.
fn range(body: &str, cap: usize) -> Option<Vec<String>> {
    let (from, to) = body.split_once("..")?;
    if let (Ok(from), Ok(to)) = (from.parse::<i64>(), to.parse::<i64>()) {
        let (low, high) = (from.min(to), from.max(to));
        return Some(
            (low..=high)
                .take(cap.saturating_add(1))
                .map(|n| n.to_string())
                .collect(),
        );
    }
    let (mut from, mut to) = (from.chars(), to.chars());
    match (from.next(), from.next(), to.next(), to.next()) {
        (Some(a), None, Some(b), None) if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
            let (low, high) = (a.min(b), a.max(b));
            Some((low..=high).map(String::from).collect())
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod shell_tests;
