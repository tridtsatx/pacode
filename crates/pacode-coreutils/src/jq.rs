//! `jq`: a pragmatic subset implemented over `serde_json`.
//!
//! Supported: `.a.b`, `.[]`, `.[N]`, `.[a:b]`, `?`, `|`, `,`, `select`, `map`, `length`, `keys`,
//! arithmetic/comparison/`and`/`or`/`//`, `if/elif/else/end`, array/object construction,
//! `$var` (via `--arg`/`--argjson`), `@csv`/`@tsv`, and common builtins (`sort_by`, `group_by`,
//! `unique`, `add`, `join`, `split`, `test`, `has`, `contains`, `to_entries`, `range`, ...).
//! Not supported: `def`, `reduce`/`foreach`, `as` bindings, assignment/update operators,
//! string interpolation, `limit`-style generators beyond those listed.
//! Flags: `-r -j -c -s -n -e --arg --argjson`.

use std::path::Path;

use serde_json::Value;

use crate::args::{Flag, parse, sw};
use crate::io::inputs;
use crate::jq_eval::{Vars, eval, truthy};
use crate::jq_parse::parse_filter;
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "jq_tests.rs"]
mod jq_tests;

/// Remove `--arg NAME VALUE` / `--argjson NAME JSON` pairs, returning the variables.
fn extract_vars(args: &[String]) -> Result<(Vec<String>, Vars), UtilError> {
    let mut rest = Vec::new();
    let mut vars = Vars::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--arg" || arg == "--argjson" {
            let (Some(name), Some(value)) = (args.get(i), args.get(i + 1)) else {
                return Err(UtilError::usage(format!("{arg} requires NAME and VALUE")));
            };
            i += 2;
            let v = if arg == "--arg" {
                Value::String(value.clone())
            } else {
                serde_json::from_str(value)
                    .map_err(|e| UtilError::usage(format!("--argjson {name}: invalid JSON: {e}")))?
            };
            vars.insert(name.clone(), v);
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((rest, vars))
}

/// `jq [-r] [-j] [-c] [-s] [-n] [-e] [--arg N V] [--argjson N J] FILTER [FILE...]`
pub fn jq(args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("r", 'r', "raw-output"),
        sw("j", 'j', "join-output"),
        sw("c", 'c', "compact-output"),
        sw("s", 's', "slurp"),
        sw("n", 'n', "null-input"),
        sw("e", 'e', "exit-status"),
    ];
    let (rest, vars) = extract_vars(args)?;
    let p = parse(&rest, SPEC, false)?;
    let Some((filter_text, files)) = p.operands.split_first() else {
        return Err(UtilError::usage("missing filter"));
    };
    let ast =
        parse_filter(filter_text).map_err(|e| UtilError::usage(format!("compile error: {e}")))?;

    let mut values: Vec<Value> = Vec::new();
    if p.has("n") {
        values.push(Value::Null);
    } else {
        for input in inputs(files, stdin, cwd)? {
            let stream = serde_json::Deserializer::from_slice(&input.data).into_iter::<Value>();
            for item in stream {
                values.push(
                    item.map_err(|e| {
                        UtilError::failed(format!("{}: parse error: {e}", input.name))
                    })?,
                );
            }
        }
        if p.has("s") {
            values = vec![Value::Array(values)];
        }
    }

    let raw = p.has("r") || p.has("j");
    let join = p.has("j");
    let mut out = String::new();
    let mut errors = String::new();
    let mut last: Option<Value> = None;
    for value in &values {
        match eval(&ast, value, &vars) {
            Ok(results) => {
                for result in results {
                    match (&result, raw) {
                        (Value::String(s), true) => out.push_str(s),
                        _ if p.has("c") => out.push_str(
                            &serde_json::to_string(&result)
                                .map_err(|e| UtilError::failed(e.to_string()))?,
                        ),
                        _ => out.push_str(
                            &serde_json::to_string_pretty(&result)
                                .map_err(|e| UtilError::failed(e.to_string()))?,
                        ),
                    }
                    if !join {
                        out.push('\n');
                    }
                    last = Some(result);
                }
            }
            Err(message) => errors.push_str(&format!("jq: error: {message}\n")),
        }
    }
    let code = if !errors.is_empty() {
        5
    } else if p.has("e") {
        match &last {
            None => 4,
            Some(v) if !truthy(v) => 1,
            Some(_) => 0,
        }
    } else {
        0
    };
    let mut result = UtilOutput::with_code(out, code);
    result.stderr = errors;
    Ok(result)
}
