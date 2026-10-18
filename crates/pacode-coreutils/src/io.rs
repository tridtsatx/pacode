//! Input helpers: files, stdin, and line splitting.

use std::path::{Path, PathBuf};

use crate::{MAX_INPUT_BYTES, UtilError};

pub(crate) fn resolve(cwd: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub(crate) fn io_error(name: &str, error: &std::io::Error) -> UtilError {
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => error.to_string(),
    };
    UtilError::failed(format!("{name}: {reason}"))
}

pub(crate) fn read_file(cwd: &Path, name: &str) -> Result<Vec<u8>, UtilError> {
    let path = resolve(cwd, name);
    let meta = std::fs::metadata(&path).map_err(|e| io_error(name, &e))?;
    if meta.is_dir() {
        return Err(UtilError::failed(format!("{name}: Is a directory")));
    }
    if !meta.is_file() {
        return Err(UtilError::failed(format!("{name}: not a regular file")));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(UtilError::failed(format!(
            "{name}: file too large ({} bytes, limit {MAX_INPUT_BYTES})",
            meta.len()
        )));
    }
    std::fs::read(&path).map_err(|e| io_error(name, &e))
}

pub(crate) struct Input {
    /// File name as given, or `-` for stdin.
    pub name: String,
    pub data: Vec<u8>,
}

fn stdin_input(stdin: Option<&[u8]>) -> Result<Input, UtilError> {
    match stdin {
        Some(data) if data.len() as u64 <= MAX_INPUT_BYTES => Ok(Input {
            name: "-".to_string(),
            data: data.to_vec(),
        }),
        Some(_) => Err(UtilError::failed("standard input too large")),
        None => Err(UtilError::usage(
            "missing input: give a file operand or pipe data in",
        )),
    }
}

/// Read every operand (`-` is stdin); with no operands read stdin.
pub(crate) fn inputs(
    operands: &[String],
    stdin: Option<&[u8]>,
    cwd: &Path,
) -> Result<Vec<Input>, UtilError> {
    if operands.is_empty() {
        return Ok(vec![stdin_input(stdin)?]);
    }
    let mut out = Vec::new();
    for operand in operands {
        if operand == "-" {
            out.push(stdin_input(stdin)?);
        } else {
            out.push(Input {
                name: operand.clone(),
                data: read_file(cwd, operand)?,
            });
        }
    }
    Ok(out)
}

/// All inputs concatenated (a newline is inserted between files lacking one), as text.
pub(crate) fn joined_text(
    operands: &[String],
    stdin: Option<&[u8]>,
    cwd: &Path,
) -> Result<String, UtilError> {
    let mut bytes: Vec<u8> = Vec::new();
    for input in inputs(operands, stdin, cwd)? {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(&input.data);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Lines of `text` split on `\n` only (a `\r` stays on its line); no trailing empty line.
pub(crate) fn text_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Byte spans `(start, end)` of each line; `end` includes the `\n` when present.
pub(crate) fn line_spans(data: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, byte) in data.iter().enumerate() {
        if *byte == b'\n' {
            spans.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < data.len() {
        spans.push((start, data.len()));
    }
    spans
}
