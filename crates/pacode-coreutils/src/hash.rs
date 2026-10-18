//! `sha256sum`, `md5sum`, `base64`.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::{inputs, read_file, text_lines};
use crate::md5::md5_hex;
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "hash_tests.rs"]
mod hash_tests;

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn checksum(
    args: &[String],
    stdin: Option<&[u8]>,
    cwd: &Path,
    tool: &str,
    hex: fn(&[u8]) -> String,
) -> UtilResult {
    const SPEC: &[Flag] = &[sw("c", 'c', "check")];
    let p = parse(args, SPEC, false)?;
    if !p.has("c") {
        let mut out = String::new();
        for input in inputs(&p.operands, stdin, cwd)? {
            out.push_str(&format!("{}  {}\n", hex(&input.data), input.name));
        }
        return Ok(UtilOutput::ok(out));
    }
    let mut out = String::new();
    let mut failed = 0usize;
    let mut checked = 0usize;
    for list in inputs(&p.operands, stdin, cwd)? {
        let text = String::from_utf8_lossy(&list.data).into_owned();
        for line in text_lines(&text) {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let Some((expected, rest)) = line.split_once(char::is_whitespace) else {
                return Err(UtilError::failed(format!(
                    "{}: improperly formatted checksum line",
                    list.name
                )));
            };
            let name = rest.trim_start().trim_start_matches('*');
            checked += 1;
            match read_file(cwd, name) {
                Ok(data) if hex(&data).eq_ignore_ascii_case(expected) => {
                    out.push_str(&format!("{name}: OK\n"));
                }
                Ok(_) => {
                    failed += 1;
                    out.push_str(&format!("{name}: FAILED\n"));
                }
                Err(_) => {
                    failed += 1;
                    out.push_str(&format!("{name}: FAILED open or read\n"));
                }
            }
        }
    }
    let mut result = UtilOutput::with_code(out, if failed > 0 { 1 } else { 0 });
    if failed > 0 {
        result.stderr =
            format!("{tool}: WARNING: {failed} of {checked} computed checksums did NOT match\n");
    }
    Ok(result)
}

/// `sha256sum [-c] [FILE...]`
pub fn sha256sum(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    checksum(args, stdin, cwd, "sha256sum", sha256_hex)
}

/// `md5sum [-c] [FILE...]`
pub fn md5sum(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    checksum(args, stdin, cwd, "md5sum", md5_hex)
}

/// `base64 [-d] [-w COLS] [FILE]`
pub fn base64(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[sw("d", 'd', "decode"), val("w", 'w', "wrap")];
    let p = parse(args, SPEC, false)?;
    if p.operands.len() > 1 {
        return Err(UtilError::usage("expected at most one input"));
    }
    let wrap = match p.get("w") {
        Some(w) => parse_usize("wrap width", w)?,
        None => 76,
    };
    let data = match inputs(&p.operands, stdin, cwd)?.into_iter().next() {
        Some(input) => input.data,
        None => Vec::new(),
    };
    if p.has("d") {
        let cleaned: Vec<u8> = data
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        let decoded = STANDARD
            .decode(&cleaned)
            .map_err(|e| UtilError::failed(format!("invalid input: {e}")))?;
        return Ok(UtilOutput::ok(decoded));
    }
    let encoded = STANDARD.encode(&data);
    if wrap == 0 {
        return Ok(UtilOutput::ok(encoded));
    }
    let mut out = String::with_capacity(encoded.len() + encoded.len() / wrap + 1);
    let bytes = encoded.as_bytes();
    for chunk in bytes.chunks(wrap) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    Ok(UtilOutput::ok(out))
}
