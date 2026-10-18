//! `diff [-u] [-U N] [-q] FILE1 FILE2`: unified diffs via `similar`.

use std::path::Path;

use similar::TextDiff;

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::inputs;
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "diff_tests.rs"]
mod diff_tests;

/// Exit 0 when identical, 1 when different (output is the unified diff).
pub fn diff(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("u", 'u', ""),
        val("U", 'U', "unified"),
        sw("q", 'q', "brief"),
    ];
    let p = parse(args, SPEC, false)?;
    if p.operands.len() != 2 {
        return Err(UtilError::usage("expected exactly two files: FILE1 FILE2"));
    }
    let context = match p.get("U") {
        Some(n) => parse_usize("context length", n)?,
        None => 3,
    };
    let ins = inputs(&p.operands, stdin, cwd)?;
    let (a, b) = (&ins[0], &ins[1]);
    if a.data == b.data {
        return Ok(UtilOutput::ok(Vec::new()));
    }
    if a.data.contains(&0) || b.data.contains(&0) {
        return Ok(UtilOutput::with_code(
            format!("Binary files {} and {} differ\n", a.name, b.name),
            1,
        ));
    }
    if p.has("q") {
        return Ok(UtilOutput::with_code(
            format!("Files {} and {} differ\n", a.name, b.name),
            1,
        ));
    }
    let old = String::from_utf8_lossy(&a.data);
    let new = String::from_utf8_lossy(&b.data);
    let old_text: &str = &old;
    let new_text: &str = &new;
    let text_diff = TextDiff::from_lines(old_text, new_text);
    let mut unified = text_diff.unified_diff();
    unified.context_radius(context).header(&a.name, &b.name);
    Ok(UtilOutput::with_code(unified.to_string(), 1))
}
