//! `paste` and `join` (hash join: inputs need not be sorted).

use std::collections::HashMap;
use std::path::Path;

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::{inputs, read_file, text_lines};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "merge_tests.rs"]
mod merge_tests;

fn unescape(text: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn owned_lines(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    text_lines(&text).into_iter().map(String::from).collect()
}

/// `paste [-s] [-d LIST] [FILE...]` (`-` repeated shares stdin round-robin).
pub fn paste(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[val("d", 'd', "delimiters"), sw("s", 's', "serial")];
    let p = parse(args, SPEC, false)?;
    let delims = unescape(p.get("d").unwrap_or("\t"));
    if delims.is_empty() {
        return Err(UtilError::usage("the delimiter list cannot be empty"));
    }
    let operands: Vec<String> = if p.operands.is_empty() {
        vec!["-".to_string()]
    } else {
        p.operands.clone()
    };
    let dashes = operands.iter().filter(|o| *o == "-").count();
    let stdin_lines = if dashes > 0 {
        match stdin {
            Some(data) => owned_lines(data),
            None => {
                return Err(UtilError::usage("missing input: `-` needs piped data"));
            }
        }
    } else {
        Vec::new()
    };
    let mut columns: Vec<Vec<String>> = Vec::new();
    let mut dash_seen = 0;
    for operand in &operands {
        if operand == "-" {
            columns.push(
                stdin_lines
                    .iter()
                    .skip(dash_seen)
                    .step_by(dashes)
                    .cloned()
                    .collect(),
            );
            dash_seen += 1;
        } else {
            columns.push(owned_lines(&read_file(cwd, operand)?));
        }
    }
    let mut out = String::new();
    if p.has("s") {
        for column in &columns {
            for (i, cell) in column.iter().enumerate() {
                if i > 0 {
                    out.push(delims[(i - 1) % delims.len()]);
                }
                out.push_str(cell);
            }
            out.push('\n');
        }
    } else {
        let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
        for r in 0..rows {
            for (ci, column) in columns.iter().enumerate() {
                if ci > 0 {
                    out.push(delims[(ci - 1) % delims.len()]);
                }
                if let Some(cell) = column.get(r) {
                    out.push_str(cell);
                }
            }
            out.push('\n');
        }
    }
    Ok(UtilOutput::ok(out))
}

fn split_fields(line: &str, delim: Option<char>) -> Vec<String> {
    match delim {
        Some(d) => line.split(d).map(String::from).collect(),
        None => line.split_whitespace().map(String::from).collect(),
    }
}

/// `join [-t C] [-1 N] [-2 N] FILE1 FILE2`: inner join on a field, in FILE1 order.
pub fn join(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[val("t", 't', ""), val("f1", '1', ""), val("f2", '2', "")];
    let p = parse(args, SPEC, false)?;
    if p.operands.len() != 2 {
        return Err(UtilError::usage("expected exactly two inputs: FILE1 FILE2"));
    }
    let delim = match p.get("t") {
        Some(t) => {
            let mut it = t.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Some(c),
                _ => return Err(UtilError::usage("the delimiter must be a single character")),
            }
        }
        None => None,
    };
    let field = |id: &str| -> Result<usize, UtilError> {
        match p.get(id) {
            Some(v) => {
                let n = parse_usize("field number", v)?;
                if n == 0 {
                    return Err(UtilError::usage("fields are numbered from 1"));
                }
                Ok(n - 1)
            }
            None => Ok(0),
        }
    };
    let (k1, k2) = (field("f1")?, field("f2")?);
    let ins = inputs(&p.operands, stdin, cwd)?;
    let sep = delim.map(String::from).unwrap_or_else(|| " ".to_string());
    let rest = |fields: &[String], key: usize| -> Vec<String> {
        fields
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != key)
            .map(|(_, f)| f.clone())
            .collect()
    };

    let right_text = String::from_utf8_lossy(&ins[1].data).into_owned();
    let mut right: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    for line in text_lines(&right_text) {
        let fields = split_fields(line, delim);
        let key = fields.get(k2).cloned().unwrap_or_default();
        right.entry(key).or_default().push(rest(&fields, k2));
    }
    let left_text = String::from_utf8_lossy(&ins[0].data).into_owned();
    let mut out = String::new();
    for line in text_lines(&left_text) {
        let fields = split_fields(line, delim);
        let key = fields.get(k1).cloned().unwrap_or_default();
        let Some(matches) = right.get(&key) else {
            continue;
        };
        let left_rest = rest(&fields, k1);
        for m in matches {
            let mut cells = vec![key.clone()];
            cells.extend(left_rest.iter().cloned());
            cells.extend(m.iter().cloned());
            out.push_str(&cells.join(&sep));
            out.push('\n');
        }
    }
    Ok(UtilOutput::ok(out))
}
