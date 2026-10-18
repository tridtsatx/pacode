//! `sort` and `uniq`.

use std::cmp::Ordering;
use std::path::Path;

use crate::args::{Flag, parse, sw, val};
use crate::io::{joined_text, text_lines};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "sorting_tests.rs"]
mod sorting_tests;

#[derive(Clone, Copy)]
struct Opts {
    numeric: bool,
    reverse: bool,
    fold: bool,
}

#[derive(Clone, Copy)]
struct KeySpec {
    /// Whole line instead of fields.
    whole: bool,
    start: usize,
    end: Option<usize>,
    opts: Opts,
}

fn parse_num(text: &str) -> f64 {
    let t = text.trim_start();
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    t[..i].parse::<f64>().unwrap_or(0.0)
}

/// Split `3,5nr` style text into (leading digits, trailing modifier letters).
fn split_field_spec(part: &str) -> Result<(usize, String), UtilError> {
    let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
    let mods: String = part.chars().skip(digits.len()).collect();
    if mods.contains('.') {
        return Err(UtilError::usage(
            "character offsets in -k are not supported (use FIELD[,FIELD])",
        ));
    }
    let n = digits
        .parse::<usize>()
        .map_err(|_| UtilError::usage(format!("invalid key field in '{part}'")))?;
    if n == 0 {
        return Err(UtilError::usage("key fields are numbered from 1"));
    }
    Ok((n, mods))
}

fn parse_key(spec: &str, global: Opts) -> Result<KeySpec, UtilError> {
    let (first, second) = match spec.split_once(',') {
        Some((a, b)) => (a, Some(b)),
        None => (spec, None),
    };
    let (start, mut mods) = split_field_spec(first)?;
    let end = match second {
        Some(b) => {
            let (e, m) = split_field_spec(b)?;
            mods.push_str(&m);
            Some(e)
        }
        None => None,
    };
    let opts = if mods.is_empty() {
        global
    } else {
        let mut o = Opts {
            numeric: false,
            reverse: false,
            fold: false,
        };
        for c in mods.chars() {
            match c {
                'n' => o.numeric = true,
                'r' => o.reverse = true,
                'f' => o.fold = true,
                other => {
                    return Err(UtilError::usage(format!(
                        "unsupported key modifier '{other}' (use n, r or f)"
                    )));
                }
            }
        }
        o
    };
    Ok(KeySpec {
        whole: false,
        start,
        end,
        opts,
    })
}

fn extract(line: &str, key: &KeySpec, delim: Option<char>) -> String {
    if key.whole {
        return line.to_string();
    }
    let fields: Vec<&str> = match delim {
        Some(d) => line.split(d).collect(),
        None => line.split_whitespace().collect(),
    };
    let from = key.start - 1;
    if from >= fields.len() {
        return String::new();
    }
    let to = key.end.unwrap_or(fields.len()).min(fields.len());
    if to <= from {
        return String::new();
    }
    let sep = delim.map(String::from).unwrap_or_else(|| " ".to_string());
    fields[from..to].join(&sep)
}

fn compare_key(a: &str, b: &str, opts: Opts) -> Ordering {
    let ord = if opts.numeric {
        parse_num(a)
            .partial_cmp(&parse_num(b))
            .unwrap_or(Ordering::Equal)
    } else if opts.fold {
        a.to_lowercase().cmp(&b.to_lowercase())
    } else {
        a.cmp(b)
    };
    if opts.reverse { ord.reverse() } else { ord }
}

fn parse_delim(text: &str) -> Result<char, UtilError> {
    if text == "\\t" {
        return Ok('\t');
    }
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(UtilError::usage("the delimiter must be a single character")),
    }
}

struct Item<'a> {
    line: &'a str,
    keys: Vec<String>,
}

/// `sort [-n] [-r] [-u] [-f] [-s] [-k F[,G][nrf]]... [-t C] [FILE...]`
pub fn sort(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("n", 'n', "numeric-sort"),
        sw("r", 'r', "reverse"),
        sw("u", 'u', "unique"),
        sw("f", 'f', "ignore-case"),
        sw("s", 's', "stable"),
        val("k", 'k', "key"),
        val("t", 't', "field-separator"),
    ];
    let p = parse(args, SPEC, false)?;
    let global = Opts {
        numeric: p.has("n"),
        reverse: p.has("r"),
        fold: p.has("f"),
    };
    let delim = match p.get("t") {
        Some(t) => Some(parse_delim(t)?),
        None => None,
    };
    let mut keys: Vec<KeySpec> = Vec::new();
    for spec in p.all("k") {
        keys.push(parse_key(spec, global)?);
    }
    if keys.is_empty() {
        keys.push(KeySpec {
            whole: true,
            start: 1,
            end: None,
            opts: global,
        });
    }
    let unique = p.has("u");
    let last_resort = !p.has("s") && !unique;

    let text = joined_text(&p.operands, stdin, cwd)?;
    let mut items: Vec<Item> = text_lines(&text)
        .into_iter()
        .map(|line| Item {
            line,
            keys: keys.iter().map(|k| extract(line, k, delim)).collect(),
        })
        .collect();

    let key_order = |a: &Item, b: &Item| -> Ordering {
        for (i, key) in keys.iter().enumerate() {
            let ord = compare_key(&a.keys[i], &b.keys[i], key.opts);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    };
    items.sort_by(|a, b| {
        let ord = key_order(a, b);
        if ord == Ordering::Equal && last_resort {
            let raw = a.line.cmp(b.line);
            return if global.reverse { raw.reverse() } else { raw };
        }
        ord
    });
    if unique {
        items.dedup_by(|later, earlier| key_order(earlier, later) == Ordering::Equal);
    }
    let mut out = Vec::new();
    for item in &items {
        out.extend_from_slice(item.line.as_bytes());
        out.push(b'\n');
    }
    Ok(UtilOutput::ok(out))
}

/// `uniq [-c] [-d] [-u] [-i] [FILE]`
pub fn uniq(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("c", 'c', "count"),
        sw("d", 'd', "repeated"),
        sw("u", 'u', "unique"),
        sw("i", 'i', "ignore-case"),
    ];
    let p = parse(args, SPEC, false)?;
    if p.operands.len() > 1 {
        return Err(UtilError::usage(
            "writing to an output file is not supported (at most one input operand)",
        ));
    }
    let text = joined_text(&p.operands, stdin, cwd)?;
    let fold = p.has("i");
    let same = |a: &str, b: &str| {
        if fold {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    };
    let mut groups: Vec<(&str, usize)> = Vec::new();
    for line in text_lines(&text) {
        let merge = match groups.last() {
            Some((first, _)) => same(first, line),
            None => false,
        };
        if merge {
            if let Some(last) = groups.last_mut() {
                last.1 += 1;
            }
        } else {
            groups.push((line, 1));
        }
    }
    let mut out = String::new();
    for (line, n) in groups {
        if p.has("d") && n < 2 {
            continue;
        }
        if p.has("u") && n != 1 {
            continue;
        }
        if p.has("c") {
            out.push_str(&format!("{n:>7} "));
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(UtilOutput::ok(out))
}
