//! A deliberately small `sed`: `s/re/repl/[gpI]`, `p`, `d`, with line, `$`, `/re/`
//! and `N,M` addresses (and `!`). Commands separate with `;`, newlines or repeated `-e`.
//! Regexes use Rust syntax; without `-E`/`-r` the POSIX basic-regex escapes
//! (`\(`, `\)`, `\{`, `\}`, `\|`, `\+`, `\?`) are translated. Never edits files.

use std::path::Path;

use regex::{Regex, RegexBuilder};

use crate::args::{Flag, parse, sw, val};
use crate::io::{joined_text, text_lines};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "sed_tests.rs"]
mod sed_tests;

enum Bound {
    Line(usize),
    Last,
    Re(Regex),
}

impl Bound {
    fn hit(&self, n: usize, last: bool, line: &str) -> bool {
        match self {
            Bound::Line(m) => n == *m,
            Bound::Last => last,
            Bound::Re(re) => re.is_match(line),
        }
    }

    fn ends(&self, n: usize, last: bool, line: &str) -> bool {
        match self {
            Bound::Line(m) => n >= *m,
            Bound::Last => last,
            Bound::Re(re) => re.is_match(line),
        }
    }
}

enum Addr {
    All,
    One(Bound),
    Range(Bound, Bound),
}

enum Part {
    Lit(String),
    Group(usize),
}

enum Op {
    Print,
    Delete,
    Subst {
        re: Regex,
        repl: Vec<Part>,
        global: bool,
        print: bool,
    },
}

struct Cmd {
    addr: Addr,
    negate: bool,
    op: Op,
    active: bool,
}

impl Cmd {
    fn selects(&mut self, n: usize, last: bool, line: &str) -> bool {
        let hit = match &self.addr {
            Addr::All => true,
            Addr::One(b) => b.hit(n, last, line),
            Addr::Range(a, b) => {
                if self.active {
                    if b.ends(n, last, line) {
                        self.active = false;
                    }
                    true
                } else if a.hit(n, last, line) {
                    self.active = match b {
                        Bound::Line(m) => *m > n,
                        Bound::Last => !last,
                        Bound::Re(_) => true,
                    };
                    true
                } else {
                    false
                }
            }
        };
        hit != self.negate
    }
}

/// Translate POSIX basic-regex syntax into Rust regex syntax.
fn bre_to_rust(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => match chars.get(i) {
                Some(&n @ ('(' | ')' | '{' | '}' | '|' | '+' | '?')) => {
                    out.push(n);
                    i += 1;
                }
                Some('<') | Some('>') => {
                    out.push_str("\\b");
                    i += 1;
                }
                Some(&n) => {
                    out.push('\\');
                    out.push(n);
                    i += 1;
                }
                None => out.push_str("\\\\"),
            },
            '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                out.push('\\');
                out.push(c);
            }
            '[' => {
                out.push('[');
                if chars.get(i) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
                while i < chars.len() {
                    let b = chars[i];
                    if b == '[' && chars.get(i + 1) == Some(&':') {
                        // character class like [:alpha:]
                        while i < chars.len() {
                            out.push(chars[i]);
                            i += 1;
                            if chars[i - 1] == ']' && chars.get(i - 2) == Some(&':') {
                                break;
                            }
                        }
                        continue;
                    }
                    out.push(b);
                    i += 1;
                    if b == ']' {
                        break;
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

struct Builder {
    ere: bool,
}

impl Builder {
    fn regex(&self, pattern: &str, ci: bool) -> Result<Regex, UtilError> {
        if pattern.is_empty() {
            return Err(UtilError::usage("an empty regex is not supported"));
        }
        let source = if self.ere {
            pattern.to_string()
        } else {
            bre_to_rust(pattern)
        };
        RegexBuilder::new(&source)
            .case_insensitive(ci)
            .build()
            .map_err(|e| UtilError::usage(format!("invalid regex '{pattern}': {e}")))
    }
}

struct Cur {
    chars: Vec<char>,
    pos: usize,
}

impl Cur {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_blanks(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.pos += 1;
        }
    }

    /// Read up to an unescaped `delim`; `\delim` becomes `delim`, other escapes are kept.
    fn delimited(&mut self, delim: char) -> Result<String, UtilError> {
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(UtilError::usage("unterminated `s` command or address")),
                Some(c) if c == delim => return Ok(out),
                Some('\\') => match self.bump() {
                    Some(n) if n == delim => out.push(n),
                    Some(n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => return Err(UtilError::usage("trailing backslash")),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits: String = self.chars[start..self.pos].iter().collect();
        digits.parse().ok()
    }
}

fn parse_bound(cur: &mut Cur, b: &Builder) -> Result<Option<Bound>, UtilError> {
    match cur.peek() {
        Some(c) if c.is_ascii_digit() => match cur.number() {
            Some(0) => Err(UtilError::usage("line numbers start at 1")),
            Some(n) => Ok(Some(Bound::Line(n))),
            None => Err(UtilError::usage("invalid line number")),
        },
        Some('$') => {
            cur.bump();
            Ok(Some(Bound::Last))
        }
        Some('/') => {
            cur.bump();
            let pattern = cur.delimited('/')?;
            Ok(Some(Bound::Re(b.regex(&pattern, false)?)))
        }
        _ => Ok(None),
    }
}

fn parse_replacement(raw: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '&' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(Part::Group(0));
            }
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => {
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Group(d as usize - '0' as usize));
                }
                Some('n') => lit.push('\n'),
                Some('t') => lit.push('\t'),
                Some(other) => lit.push(other),
                None => lit.push('\\'),
            },
            other => lit.push(other),
        }
    }
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    parts
}

fn parse_script(script: &str, b: &Builder) -> Result<Vec<Cmd>, UtilError> {
    let mut cur = Cur {
        chars: script.chars().collect(),
        pos: 0,
    };
    let mut cmds = Vec::new();
    loop {
        while matches!(cur.peek(), Some(' ') | Some('\t') | Some('\n') | Some(';')) {
            cur.bump();
        }
        if cur.peek().is_none() {
            break;
        }
        let addr = match parse_bound(&mut cur, b)? {
            None => Addr::All,
            Some(first) => {
                if cur.peek() == Some(',') {
                    cur.bump();
                    match parse_bound(&mut cur, b)? {
                        Some(second) => Addr::Range(first, second),
                        None => return Err(UtilError::usage("missing second address after `,`")),
                    }
                } else {
                    Addr::One(first)
                }
            }
        };
        cur.skip_blanks();
        let negate = cur.peek() == Some('!');
        if negate {
            cur.bump();
            cur.skip_blanks();
        }
        let op = match cur.bump() {
            Some('p') => Op::Print,
            Some('d') => Op::Delete,
            Some('s') => parse_subst(&mut cur, b)?,
            Some(other) => {
                return Err(UtilError::usage(format!(
                    "unsupported sed command `{other}` (supported: s, p, d)"
                )));
            }
            None => return Err(UtilError::usage("missing command")),
        };
        cur.skip_blanks();
        match cur.peek() {
            None | Some(';') | Some('\n') => {}
            Some(other) => {
                return Err(UtilError::usage(format!(
                    "unexpected `{other}` after command"
                )));
            }
        }
        cmds.push(Cmd {
            addr,
            negate,
            op,
            active: false,
        });
    }
    Ok(cmds)
}

fn parse_subst(cur: &mut Cur, b: &Builder) -> Result<Op, UtilError> {
    let delim = cur
        .bump()
        .ok_or_else(|| UtilError::usage("missing `s` delimiter"))?;
    if delim.is_alphanumeric() || matches!(delim, '\\' | '\n' | '&' | ' ') {
        return Err(UtilError::usage(format!("invalid `s` delimiter `{delim}`")));
    }
    let pattern = cur.delimited(delim)?;
    let replacement = cur.delimited(delim)?;
    let (mut global, mut print, mut ci) = (false, false, false);
    while let Some(flag) = cur.peek() {
        match flag {
            'g' => global = true,
            'p' => print = true,
            'I' | 'i' => ci = true,
            ' ' | '\t' | ';' | '\n' => break,
            other => {
                return Err(UtilError::usage(format!(
                    "unsupported `s` flag `{other}` (supported: g, p, I)"
                )));
            }
        }
        cur.bump();
    }
    Ok(Op::Subst {
        re: b.regex(&pattern, ci)?,
        repl: parse_replacement(&replacement),
        global,
        print,
    })
}

fn substitute(re: &Regex, repl: &[Part], global: bool, text: &str) -> Option<String> {
    let mut out = String::new();
    let mut last = 0;
    let mut any = false;
    for caps in re.captures_iter(text) {
        let Some(whole) = caps.get(0) else {
            continue;
        };
        any = true;
        out.push_str(&text[last..whole.start()]);
        for part in repl {
            match part {
                Part::Lit(s) => out.push_str(s),
                Part::Group(i) => out.push_str(caps.get(*i).map(|m| m.as_str()).unwrap_or("")),
            }
        }
        last = whole.end();
        if !global {
            break;
        }
    }
    if !any {
        return None;
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// `sed [-n] [-E|-r] (-e SCRIPT)... | SCRIPT [FILE...]`
pub fn sed(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("n", 'n', "quiet"),
        sw("n", '\0', "silent"),
        sw("E", 'E', "regexp-extended"),
        sw("E", 'r', ""),
        val("e", 'e', "expression"),
    ];
    let p = parse(args, SPEC, false)?;
    let scripts = p.all("e");
    let (script, files): (String, Vec<String>) = if scripts.is_empty() {
        match p.operands.split_first() {
            Some((first, rest)) => (first.clone(), rest.to_vec()),
            None => return Err(UtilError::usage("missing script")),
        }
    } else {
        (scripts.join("\n"), p.operands.clone())
    };
    let builder = Builder { ere: p.has("E") };
    let mut cmds = parse_script(&script, &builder)?;
    let quiet = p.has("n");

    let text = joined_text(&files, stdin, cwd)?;
    let lines = text_lines(&text);
    let total = lines.len();
    let mut out = String::new();
    let mut last_autoprinted = false;
    for (idx, original) in lines.iter().enumerate() {
        let n = idx + 1;
        let is_last = n == total;
        let mut line = (*original).to_string();
        let mut deleted = false;
        for cmd in cmds.iter_mut() {
            if !cmd.selects(n, is_last, &line) {
                continue;
            }
            match &cmd.op {
                Op::Print => {
                    out.push_str(&line);
                    out.push('\n');
                }
                Op::Delete => {
                    deleted = true;
                    break;
                }
                Op::Subst {
                    re,
                    repl,
                    global,
                    print,
                } => {
                    if let Some(new_line) = substitute(re, repl, *global, &line) {
                        line = new_line;
                        if *print {
                            out.push_str(&line);
                            out.push('\n');
                        }
                    }
                }
            }
        }
        if !deleted && !quiet {
            out.push_str(&line);
            out.push('\n');
            last_autoprinted = is_last;
        }
    }
    if last_autoprinted && !text.ends_with('\n') {
        out.pop();
    }
    Ok(UtilOutput::ok(out))
}
