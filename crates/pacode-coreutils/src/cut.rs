//! `cut -d D -f LIST` and `cut -c LIST`.

use std::path::Path;

use crate::args::{Flag, parse, sw, val};
use crate::io::{joined_text, text_lines};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "cut_tests.rs"]
mod cut_tests;

/// Parse `1,3-5,7-,-3` into 1-based inclusive ranges.
fn parse_list(text: &str) -> Result<Vec<(usize, usize)>, UtilError> {
    let bad = || UtilError::usage(format!("invalid field or position list: '{text}'"));
    let mut ranges = Vec::new();
    for part in text.split(',') {
        let (lo, hi) = match part.split_once('-') {
            None => {
                let n = part.parse::<usize>().map_err(|_| bad())?;
                (n, n)
            }
            Some((a, b)) => {
                let lo = if a.is_empty() {
                    1
                } else {
                    a.parse::<usize>().map_err(|_| bad())?
                };
                let hi = if b.is_empty() {
                    usize::MAX
                } else {
                    b.parse::<usize>().map_err(|_| bad())?
                };
                (lo, hi)
            }
        };
        if lo == 0 || hi == 0 || lo > hi {
            return Err(bad());
        }
        ranges.push((lo, hi));
    }
    Ok(ranges)
}

fn selected(ranges: &[(usize, usize)], position: usize) -> bool {
    ranges
        .iter()
        .any(|(lo, hi)| position >= *lo && position <= *hi)
}

/// `cut (-f LIST [-d C] [-s] | -c LIST) [--output-delimiter=S] [FILE...]`
pub fn cut(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        val("d", 'd', "delimiter"),
        val("f", 'f', "fields"),
        val("c", 'c', "characters"),
        sw("s", 's', "only-delimited"),
        val("od", '\0', "output-delimiter"),
    ];
    let p = parse(args, SPEC, false)?;
    let fields = p.get("f");
    let chars = p.get("c");
    if fields.is_some() == chars.is_some() {
        return Err(UtilError::usage("specify exactly one of -f or -c"));
    }
    let delim = match p.get("d") {
        Some(d) => {
            let mut it = d.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => c,
                _ => return Err(UtilError::usage("the delimiter must be a single character")),
            }
        }
        None => '\t',
    };
    if chars.is_some() && (p.has("d") || p.has("s")) {
        return Err(UtilError::usage("-d and -s only apply with -f"));
    }
    let out_delim = p
        .get("od")
        .map(str::to_string)
        .unwrap_or_else(|| delim.to_string());
    let text = joined_text(&p.operands, stdin, cwd)?;
    let mut out = String::new();
    if let Some(list) = fields {
        let ranges = parse_list(list)?;
        for line in text_lines(&text) {
            if !line.contains(delim) {
                if !p.has("s") {
                    out.push_str(line);
                    out.push('\n');
                }
                continue;
            }
            let picked: Vec<&str> = line
                .split(delim)
                .enumerate()
                .filter(|(i, _)| selected(&ranges, i + 1))
                .map(|(_, f)| f)
                .collect();
            out.push_str(&picked.join(&out_delim));
            out.push('\n');
        }
    } else if let Some(list) = chars {
        let ranges = parse_list(list)?;
        for line in text_lines(&text) {
            for (i, c) in line.chars().enumerate() {
                if selected(&ranges, i + 1) {
                    out.push(c);
                }
            }
            out.push('\n');
        }
    }
    Ok(UtilOutput::ok(out))
}
