//! `xxd [-l LEN] [-s SEEK] [-c COLS] [-p] [FILE]` (hexdump only; no `-r`).

use std::path::Path;

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::inputs;
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "xxd_tests.rs"]
mod xxd_tests;

pub fn xxd(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        val("l", 'l', "len"),
        val("s", 's', "seek"),
        val("c", 'c', "cols"),
        sw("p", 'p', "plain"),
    ];
    let p = parse(args, SPEC, false)?;
    if p.operands.len() > 1 {
        return Err(UtilError::usage("expected at most one input"));
    }
    let cols = match p.get("c") {
        Some(c) => parse_usize("column count", c)?,
        None => 16,
    };
    if cols == 0 || cols > 256 {
        return Err(UtilError::usage("column count must be between 1 and 256"));
    }
    let seek = match p.get("s") {
        Some(s) => parse_usize("seek offset", s)?,
        None => 0,
    };
    let data = match inputs(&p.operands, stdin, cwd)?.into_iter().next() {
        Some(input) => input.data,
        None => Vec::new(),
    };
    let start = seek.min(data.len());
    let end = match p.get("l") {
        Some(l) => start
            .saturating_add(parse_usize("length", l)?)
            .min(data.len()),
        None => data.len(),
    };
    let slice = &data[start..end];
    let mut out = String::new();
    if p.has("p") {
        for chunk in slice.chunks(30) {
            for byte in chunk {
                out.push_str(&format!("{byte:02x}"));
            }
            out.push('\n');
        }
        return Ok(UtilOutput::ok(out));
    }
    let hex_width = cols * 2 + cols.div_ceil(2) - 1;
    for (row, chunk) in slice.chunks(cols).enumerate() {
        let mut hex = String::new();
        for (i, byte) in chunk.iter().enumerate() {
            hex.push_str(&format!("{byte:02x}"));
            if i % 2 == 1 && i + 1 < chunk.len() {
                hex.push(' ');
            }
        }
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if (0x20..0x7f).contains(b) {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!(
            "{:08x}: {hex:<hex_width$}  {ascii}\n",
            start + row * cols
        ));
    }
    Ok(UtilOutput::ok(out))
}
