//! `wc`, `head`, `tail`, `tac`, `rev`, `nl`.

use std::path::Path;

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::{Input, inputs, line_spans, text_lines};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "lines_tests.rs"]
mod lines_tests;

fn display_name(input: &Input) -> &str {
    if input.name == "-" {
        "standard input"
    } else {
        &input.name
    }
}

// ---------------------------------------------------------------- wc

struct Counts {
    lines: u64,
    words: u64,
    chars: u64,
    bytes: u64,
    max_line: u64,
}

fn count(data: &[u8]) -> Counts {
    let mut words = 0u64;
    let mut in_word = false;
    let mut lines = 0u64;
    for byte in data {
        if *byte == b'\n' {
            lines += 1;
        }
        if byte.is_ascii_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
        }
    }
    let text = String::from_utf8_lossy(data);
    let chars = text.chars().count() as u64;
    let max_line = text
        .split('\n')
        .map(|l| l.chars().count() as u64)
        .max()
        .unwrap_or(0);
    Counts {
        lines,
        words,
        chars,
        bytes: data.len() as u64,
        max_line,
    }
}

/// `wc [-l] [-w] [-c] [-m] [-L] [FILE...]`
pub fn wc(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("l", 'l', "lines"),
        sw("w", 'w', "words"),
        sw("c", 'c', "bytes"),
        sw("m", 'm', "chars"),
        sw("L", 'L', "max-line-length"),
    ];
    let p = parse(args, SPEC, false)?;
    let any = ["l", "w", "c", "m", "L"].iter().any(|id| p.has(id));
    let (l, w, c, m, mx) = if any {
        (p.has("l"), p.has("w"), p.has("c"), p.has("m"), p.has("L"))
    } else {
        (true, true, true, false, false)
    };
    let ins = inputs(&p.operands, stdin, cwd)?;
    let named = !p.operands.is_empty();
    let mut rows: Vec<(Option<String>, Vec<u64>)> = Vec::new();
    let mut total = Counts {
        lines: 0,
        words: 0,
        chars: 0,
        bytes: 0,
        max_line: 0,
    };
    for input in &ins {
        let n = count(&input.data);
        total.lines += n.lines;
        total.words += n.words;
        total.chars += n.chars;
        total.bytes += n.bytes;
        total.max_line = total.max_line.max(n.max_line);
        rows.push((
            named.then(|| input.name.clone()),
            columns(&n, l, w, m, c, mx),
        ));
    }
    if rows.len() > 1 {
        rows.push((Some("total".to_string()), columns(&total, l, w, m, c, mx)));
    }
    let ncols = rows.first().map(|r| r.1.len()).unwrap_or(0);
    let width = if ncols == 1 && rows.len() == 1 {
        0
    } else {
        rows.iter()
            .flat_map(|r| r.1.iter())
            .map(|n| n.to_string().len())
            .max()
            .unwrap_or(1)
    };
    let mut out = String::new();
    for (name, nums) in rows {
        let cells: Vec<String> = nums.iter().map(|n| format!("{n:>width$}")).collect();
        out.push_str(&cells.join(" "));
        if let Some(name) = name {
            out.push(' ');
            out.push_str(&name);
        }
        out.push('\n');
    }
    Ok(UtilOutput::ok(out))
}

fn columns(n: &Counts, l: bool, w: bool, m: bool, c: bool, mx: bool) -> Vec<u64> {
    let mut cols = Vec::new();
    if l {
        cols.push(n.lines);
    }
    if w {
        cols.push(n.words);
    }
    if m {
        cols.push(n.chars);
    }
    if c {
        cols.push(n.bytes);
    }
    if mx {
        cols.push(n.max_line);
    }
    cols
}

// ------------------------------------------------------- head / tail

#[derive(Clone, Copy)]
enum Sel {
    First(usize),
    AllButLast(usize),
    Last(usize),
    From(usize),
}

fn select(data: &[u8], sel: Sel, bytes: bool) -> &[u8] {
    let len = data.len();
    if bytes {
        return match sel {
            Sel::First(n) => &data[..n.min(len)],
            Sel::AllButLast(k) => &data[..len.saturating_sub(k)],
            Sel::Last(n) => &data[len - n.min(len)..],
            Sel::From(n) => &data[n.saturating_sub(1).min(len)..],
        };
    }
    let spans = line_spans(data);
    match sel {
        Sel::First(n) => {
            let n = n.min(spans.len());
            if n == 0 {
                &data[..0]
            } else {
                &data[..spans[n - 1].1]
            }
        }
        Sel::AllButLast(k) => select(data, Sel::First(spans.len().saturating_sub(k)), false),
        Sel::Last(n) => {
            let n = n.min(spans.len());
            if n == 0 {
                &data[len..]
            } else {
                &data[spans[spans.len() - n].0..]
            }
        }
        Sel::From(n) => {
            let idx = n.saturating_sub(1);
            if idx >= spans.len() {
                &data[len..]
            } else {
                &data[spans[idx].0..]
            }
        }
    }
}

fn split_sign(text: &str) -> (Option<char>, &str) {
    match text.chars().next() {
        Some(c @ ('+' | '-')) => (Some(c), &text[1..]),
        _ => (None, text),
    }
}

fn head_tail(args: &[String], stdin: Option<&[u8]>, cwd: &Path, head: bool) -> UtilResult {
    const SPEC: &[Flag] = &[
        val("n", 'n', "lines"),
        val("c", 'c', "bytes"),
        sw("q", 'q', "quiet"),
        sw("q", '\0', "silent"),
        sw("v", 'v', "verbose"),
    ];
    let p = parse(args, SPEC, true)?;
    let bytes = p.has("c");
    let (what, spec) = if bytes {
        ("byte count", p.get("c"))
    } else {
        ("line count", p.get("n").or_else(|| p.get("num")))
    };
    let spec = spec.unwrap_or("10");
    let (sign, digits) = split_sign(spec);
    let n = parse_usize(what, digits)?;
    let sel = match (head, sign) {
        (true, Some('-')) => Sel::AllButLast(n),
        (true, _) => Sel::First(n),
        (false, Some('+')) => Sel::From(n),
        (false, _) => Sel::Last(n),
    };
    let ins = inputs(&p.operands, stdin, cwd)?;
    let headers = (ins.len() > 1 && !p.has("q")) || p.has("v");
    let mut out: Vec<u8> = Vec::new();
    for (i, input) in ins.iter().enumerate() {
        if headers {
            if i > 0 {
                if !out.ends_with(b"\n") {
                    out.push(b'\n');
                }
                out.push(b'\n');
            }
            out.extend_from_slice(format!("==> {} <==\n", display_name(input)).as_bytes());
        }
        out.extend_from_slice(select(&input.data, sel, bytes));
    }
    Ok(UtilOutput::ok(out))
}

/// `head [-n N | -N | -n -K] [-c N] [-q] [-v] [FILE...]`
pub fn head(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    head_tail(args, stdin, cwd, true)
}

/// `tail [-n N | -N | -n +K] [-c N] [-q] [-v] [FILE...]` (no `-f`)
pub fn tail(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    head_tail(args, stdin, cwd, false)
}

// --------------------------------------------------------- tac / rev

fn no_flags(args: &[String]) -> Result<Vec<String>, UtilError> {
    let p = parse(args, &[], false)?;
    Ok(p.operands)
}

/// `tac [FILE...]`: lines in reverse order, each newline-terminated.
pub fn tac(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    let operands = no_flags(args)?;
    let mut out = Vec::new();
    for input in inputs(&operands, stdin, cwd)? {
        let mut lines: Vec<&[u8]> = Vec::new();
        for (start, end) in line_spans(&input.data) {
            let line = &input.data[start..end];
            lines.push(line.strip_suffix(b"\n").unwrap_or(line));
        }
        for line in lines.iter().rev() {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    Ok(UtilOutput::ok(out))
}

/// `rev [FILE...]`: reverse the characters of every line.
pub fn rev(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    let operands = no_flags(args)?;
    let mut out = Vec::new();
    for input in inputs(&operands, stdin, cwd)? {
        let text = String::from_utf8_lossy(&input.data);
        for line in text_lines(&text) {
            let reversed: String = line.chars().rev().collect();
            out.extend_from_slice(reversed.as_bytes());
            out.push(b'\n');
        }
    }
    Ok(UtilOutput::ok(out))
}

// ---------------------------------------------------------------- nl

/// `nl [-b a|t|n] [-n ln|rn|rz] [-w N] [-s SEP] [-v N] [-i N] [FILE...]`
pub fn nl(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        val("b", 'b', "body-numbering"),
        val("n", 'n', "number-format"),
        val("w", 'w', "number-width"),
        val("s", 's', "number-separator"),
        val("v", 'v', "starting-line-number"),
        val("i", 'i', "line-increment"),
    ];
    let p = parse(args, SPEC, false)?;
    let style = p.get("b").unwrap_or("t");
    if !matches!(style, "a" | "t" | "n") {
        return Err(UtilError::usage(format!(
            "unsupported body numbering style '{style}' (use a, t or n)"
        )));
    }
    let format = p.get("n").unwrap_or("rn");
    if !matches!(format, "ln" | "rn" | "rz") {
        return Err(UtilError::usage(format!(
            "invalid number format '{format}' (use ln, rn or rz)"
        )));
    }
    let width = match p.get("w") {
        Some(w) => parse_usize("number width", w)?.min(64),
        None => 6,
    };
    let sep = p.get("s").unwrap_or("\t");
    let mut number: i64 = match p.get("v") {
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| UtilError::usage(format!("invalid starting line number: '{v}'")))?,
        None => 1,
    };
    let step: i64 = match p.get("i") {
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| UtilError::usage(format!("invalid line increment: '{v}'")))?,
        None => 1,
    };
    let mut out = String::new();
    for input in inputs(&p.operands, stdin, cwd)? {
        let text = String::from_utf8_lossy(&input.data);
        for line in text_lines(&text) {
            let numbered = match style {
                "a" => true,
                "t" => !line.is_empty(),
                _ => false,
            };
            if numbered {
                let cell = match format {
                    "ln" => format!("{number:<width$}"),
                    "rz" => format!("{number:0>width$}"),
                    _ => format!("{number:>width$}"),
                };
                out.push_str(&cell);
                out.push_str(sep);
                number = number.saturating_add(step);
            } else {
                out.push_str(&" ".repeat(width + sep.chars().count()));
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(UtilOutput::ok(out))
}
