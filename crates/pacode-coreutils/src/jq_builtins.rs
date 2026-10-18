//! Builtin functions of the supported `jq` subset.

use std::cmp::Ordering;

use serde_json::{Map, Value};

use crate::jq_eval::{
    MAX_RESULTS, Res, Vars, arith_add, cmp_values, compact_json, eval, iterate_values, num,
    regex_test, split_text, text_of, truthy, type_name,
};
use crate::jq_parse::Ast;

fn one(v: Value) -> Res {
    Ok(vec![v])
}

fn string_of<'a>(v: &'a Value, what: &str) -> Result<&'a str, String> {
    match v {
        Value::String(s) => Ok(s),
        other => Err(format!(
            "{what} requires a string, not {}",
            type_name(other)
        )),
    }
}

fn array_of<'a>(v: &'a Value, what: &str) -> Result<&'a Vec<Value>, String> {
    match v {
        Value::Array(a) => Ok(a),
        other => Err(format!(
            "{what} requires an array, not {}",
            type_name(other)
        )),
    }
}

fn length(v: &Value) -> Result<Value, String> {
    match v {
        Value::Null => Ok(Value::from(0)),
        Value::Bool(_) => Err("boolean has no length".to_string()),
        Value::Number(n) => Ok(num(n.as_f64().unwrap_or(0.0).abs())),
        Value::String(s) => Ok(Value::from(s.chars().count() as u64)),
        Value::Array(a) => Ok(Value::from(a.len() as u64)),
        Value::Object(m) => Ok(Value::from(m.len() as u64)),
    }
}

fn keys(v: &Value) -> Result<Value, String> {
    match v {
        Value::Object(m) => {
            let mut ks: Vec<&String> = m.keys().collect();
            ks.sort();
            Ok(Value::Array(
                ks.into_iter().map(|k| Value::String(k.clone())).collect(),
            ))
        }
        Value::Array(a) => Ok(Value::Array((0..a.len() as u64).map(Value::from).collect())),
        other => Err(format!("{} has no keys", type_name(other))),
    }
}

fn flatten_into(items: &[Value], depth: i64, out: &mut Vec<Value>) {
    for item in items {
        match item {
            Value::Array(inner) if depth > 0 => flatten_into(inner, depth - 1, out),
            other => out.push(other.clone()),
        }
    }
}

fn recurse_into(v: &Value, out: &mut Vec<Value>) -> Result<(), String> {
    if out.len() > MAX_RESULTS {
        return Err(format!("too many results (limit {MAX_RESULTS})"));
    }
    out.push(v.clone());
    match v {
        Value::Array(a) => {
            for item in a {
                recurse_into(item, out)?;
            }
        }
        Value::Object(m) => {
            for item in m.values() {
                recurse_into(item, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn contains(a: &Value, b: &Value) -> Result<bool, String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, bv) in y {
                match x.get(k) {
                    Some(av) => {
                        if !contains(av, bv)? {
                            return Ok(false);
                        }
                    }
                    None => return Ok(false),
                }
            }
            Ok(true)
        }
        (Value::Array(x), Value::Array(y)) => Ok(y
            .iter()
            .all(|bv| x.iter().any(|av| contains(av, bv).unwrap_or(false)))),
        (Value::String(x), Value::String(y)) => Ok(x.contains(y.as_str())),
        (x, y) if type_name(x) == type_name(y) => Ok(x == y),
        (x, y) => Err(format!(
            "{} and {} cannot have their containment checked",
            type_name(x),
            type_name(y)
        )),
    }
}

fn csv_cell(v: &Value, tsv: bool) -> Result<String, String> {
    match v {
        Value::Null => Ok(String::new()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) if tsv => Ok(s
            .replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
            .replace('\r', "\\r")),
        Value::String(s) => Ok(format!("\"{}\"", s.replace('"', "\"\""))),
        other => Err(format!(
            "{} is not valid in a csv/tsv row",
            type_name(other)
        )),
    }
}

fn sorted_pairs(arg: &Ast, input: &Value, vars: &Vars) -> Result<Vec<(Value, Value)>, String> {
    let items = array_of(input, "sort_by/group_by/unique_by")?;
    let mut pairs = Vec::with_capacity(items.len());
    for item in items {
        pairs.push((Value::Array(eval(arg, item, vars)?), item.clone()));
    }
    pairs.sort_by(|a, b| cmp_values(&a.0, &b.0));
    Ok(pairs)
}

/// Apply `f` to every value of the (cartesian) argument.
fn with_arg(
    arg: &Ast,
    input: &Value,
    vars: &Vars,
    f: impl Fn(&Value) -> Result<Value, String>,
) -> Res {
    let mut out = Vec::new();
    for a in eval(arg, input, vars)? {
        out.push(f(&a)?);
    }
    Ok(out)
}

fn range(from: f64, to: f64) -> Res {
    let count = (to - from).ceil();
    if count > MAX_RESULTS as f64 {
        return Err(format!("range is too large (limit {MAX_RESULTS})"));
    }
    let mut out = Vec::new();
    let mut x = from;
    while x < to {
        out.push(num(x));
        x += 1.0;
    }
    Ok(out)
}

pub(crate) fn call(name: &str, args: &[Ast], input: &Value, vars: &Vars) -> Res {
    match (name, args.len()) {
        ("empty", 0) => Ok(Vec::new()),
        ("not", 0) => one(Value::Bool(!truthy(input))),
        ("length", 0) => one(length(input)?),
        ("keys", 0) => one(keys(input)?),
        ("values", 0) => Ok(if input.is_null() {
            Vec::new()
        } else {
            vec![input.clone()]
        }),
        ("type", 0) => one(Value::String(type_name(input).to_string())),
        ("tostring", 0) | ("@text", 0) => one(Value::String(text_of(input))),
        ("tojson", 0) | ("@json", 0) => one(Value::String(compact_json(input))),
        ("fromjson", 0) => {
            let s = string_of(input, "fromjson")?;
            let v: Value =
                serde_json::from_str(s).map_err(|e| format!("{e} (while parsing '{s}')"))?;
            one(v)
        }
        ("tonumber", 0) => match input {
            Value::Number(_) => one(input.clone()),
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .map(|f| vec![num(f)])
                .map_err(|_| format!("Cannot parse '{s}' as number")),
            other => Err(format!("{} cannot be parsed as a number", type_name(other))),
        },
        ("ascii_downcase", 0) => one(Value::String(
            string_of(input, "ascii_downcase")?.to_ascii_lowercase(),
        )),
        ("ascii_upcase", 0) => one(Value::String(
            string_of(input, "ascii_upcase")?.to_ascii_uppercase(),
        )),
        ("floor", 0) => match input.as_f64() {
            Some(f) => one(num(f.floor())),
            None => Err(format!("{} has no floor", type_name(input))),
        },
        ("add", 0) => {
            let items = match input {
                Value::Array(a) => a.clone(),
                Value::Object(m) => m.values().cloned().collect(),
                other => return Err(format!("Cannot iterate over {}", type_name(other))),
            };
            let mut acc = Value::Null;
            for item in &items {
                acc = arith_add(&acc, item)?;
            }
            one(acc)
        }
        ("sort", 0) => {
            let mut items = array_of(input, "sort")?.clone();
            items.sort_by(cmp_values);
            one(Value::Array(items))
        }
        ("unique", 0) => {
            let mut items = array_of(input, "unique")?.clone();
            items.sort_by(cmp_values);
            items.dedup_by(|a, b| cmp_values(a, b) == Ordering::Equal);
            one(Value::Array(items))
        }
        ("reverse", 0) => match input {
            Value::Array(a) => one(Value::Array(a.iter().rev().cloned().collect())),
            Value::String(s) => one(Value::String(s.chars().rev().collect())),
            Value::Null => one(Value::Array(Vec::new())),
            other => Err(format!("Cannot reverse {}", type_name(other))),
        },
        ("min", 0) => {
            let items = array_of(input, "min")?;
            one(items
                .iter()
                .min_by(|a, b| cmp_values(a, b))
                .cloned()
                .unwrap_or(Value::Null))
        }
        ("max", 0) => {
            let items = array_of(input, "max")?;
            one(items
                .iter()
                .max_by(|a, b| cmp_values(a, b))
                .cloned()
                .unwrap_or(Value::Null))
        }
        ("first", 0) => one(array_of(input, "first")?
            .first()
            .cloned()
            .unwrap_or(Value::Null)),
        ("last", 0) => one(array_of(input, "last")?
            .last()
            .cloned()
            .unwrap_or(Value::Null)),
        ("flatten", 0) => {
            let mut out = Vec::new();
            flatten_into(array_of(input, "flatten")?, 1_000_000, &mut out);
            one(Value::Array(out))
        }
        ("flatten", 1) => {
            let items = array_of(input, "flatten")?;
            with_arg(&args[0], input, vars, |d| {
                let depth = d.as_f64().ok_or("flatten depth must be a number")?;
                if depth < 0.0 {
                    return Err("flatten depth must not be negative".to_string());
                }
                let mut out = Vec::new();
                flatten_into(items, depth as i64, &mut out);
                Ok(Value::Array(out))
            })
        }
        ("to_entries", 0) => match input {
            Value::Object(m) => one(Value::Array(
                m.iter()
                    .map(|(k, v)| {
                        let mut e = Map::new();
                        e.insert("key".to_string(), Value::String(k.clone()));
                        e.insert("value".to_string(), v.clone());
                        Value::Object(e)
                    })
                    .collect(),
            )),
            other => Err(format!("{} has no entries", type_name(other))),
        },
        ("any", 0) => one(Value::Bool(array_of(input, "any")?.iter().any(truthy))),
        ("all", 0) => one(Value::Bool(array_of(input, "all")?.iter().all(truthy))),
        ("recurse", 0) => {
            let mut out = Vec::new();
            recurse_into(input, &mut out)?;
            Ok(out)
        }
        ("@csv", 0) | ("@tsv", 0) => {
            let tsv = name == "@tsv";
            let cells: Result<Vec<String>, String> = array_of(input, name)?
                .iter()
                .map(|v| csv_cell(v, tsv))
                .collect();
            one(Value::String(cells?.join(if tsv { "\t" } else { "," })))
        }
        ("has", 1) => with_arg(&args[0], input, vars, |k| match (input, k) {
            (Value::Object(m), Value::String(key)) => Ok(Value::Bool(m.contains_key(key))),
            (Value::Array(a), Value::Number(n)) => Ok(Value::Bool(
                n.as_f64()
                    .is_some_and(|f| f >= 0.0 && (f as usize) < a.len()),
            )),
            (i, k) => Err(format!(
                "Cannot check whether {} has a key of type {}",
                type_name(i),
                type_name(k)
            )),
        }),
        ("contains", 1) => with_arg(&args[0], input, vars, |b| {
            Ok(Value::Bool(contains(input, b)?))
        }),
        ("startswith", 1) => with_arg(&args[0], input, vars, |p| {
            Ok(Value::Bool(
                string_of(input, "startswith")?.starts_with(string_of(p, "startswith")?),
            ))
        }),
        ("endswith", 1) => with_arg(&args[0], input, vars, |p| {
            Ok(Value::Bool(
                string_of(input, "endswith")?.ends_with(string_of(p, "endswith")?),
            ))
        }),
        ("ltrimstr", 1) => with_arg(&args[0], input, vars, |p| match (input, p) {
            (Value::String(s), Value::String(p)) => Ok(Value::String(
                s.strip_prefix(p.as_str()).unwrap_or(s).to_string(),
            )),
            _ => Ok(input.clone()),
        }),
        ("rtrimstr", 1) => with_arg(&args[0], input, vars, |p| match (input, p) {
            (Value::String(s), Value::String(p)) => Ok(Value::String(
                s.strip_suffix(p.as_str()).unwrap_or(s).to_string(),
            )),
            _ => Ok(input.clone()),
        }),
        ("join", 1) => with_arg(&args[0], input, vars, |sep| {
            let sep = string_of(sep, "join separator")?;
            let mut parts = Vec::new();
            for item in array_of(input, "join")? {
                parts.push(match item {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    Value::Number(_) | Value::Bool(_) => compact_json(item),
                    other => {
                        return Err(format!("cannot join {}", type_name(other)));
                    }
                });
            }
            Ok(Value::String(parts.join(sep)))
        }),
        ("split", 1) => with_arg(&args[0], input, vars, |sep| {
            Ok(split_text(
                string_of(input, "split")?,
                string_of(sep, "split")?,
            ))
        }),
        ("test", 1) => with_arg(&args[0], input, vars, |re| {
            Ok(Value::Bool(regex_test(
                string_of(input, "test")?,
                string_of(re, "test")?,
                "",
            )?))
        }),
        ("test", 2) => {
            let flags = eval(&args[1], input, vars)?;
            let flags = flags.first().map(text_of).unwrap_or_default();
            with_arg(&args[0], input, vars, |re| {
                Ok(Value::Bool(regex_test(
                    string_of(input, "test")?,
                    string_of(re, "test")?,
                    &flags,
                )?))
            })
        }
        ("range", 1) => {
            let mut out = Vec::new();
            for n in eval(&args[0], input, vars)? {
                let n = n.as_f64().ok_or("range requires numbers")?;
                out.extend(range(0.0, n)?);
            }
            Ok(out)
        }
        ("range", 2) => {
            let mut out = Vec::new();
            for a in eval(&args[0], input, vars)? {
                for b in eval(&args[1], input, vars)? {
                    let (a, b) = (
                        a.as_f64().ok_or("range requires numbers")?,
                        b.as_f64().ok_or("range requires numbers")?,
                    );
                    out.extend(range(a, b)?);
                }
            }
            Ok(out)
        }
        ("select", 1) => {
            let mut out = Vec::new();
            for c in eval(&args[0], input, vars)? {
                if truthy(&c) {
                    out.push(input.clone());
                }
            }
            Ok(out)
        }
        ("map", 1) => {
            let mut out = Vec::new();
            for item in iterate_values(input)? {
                out.extend(eval(&args[0], &item, vars)?);
                if out.len() > MAX_RESULTS {
                    return Err(format!("too many results (limit {MAX_RESULTS})"));
                }
            }
            one(Value::Array(out))
        }
        ("sort_by", 1) => {
            let pairs = sorted_pairs(&args[0], input, vars)?;
            one(Value::Array(pairs.into_iter().map(|(_, v)| v).collect()))
        }
        ("group_by", 1) => {
            let pairs = sorted_pairs(&args[0], input, vars)?;
            let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
            for (key, item) in pairs {
                let same = match groups.last() {
                    Some((last_key, _)) => cmp_values(last_key, &key) == Ordering::Equal,
                    None => false,
                };
                if same {
                    if let Some(last) = groups.last_mut() {
                        last.1.push(item);
                    }
                } else {
                    groups.push((key, vec![item]));
                }
            }
            one(Value::Array(
                groups.into_iter().map(|(_, g)| Value::Array(g)).collect(),
            ))
        }
        ("unique_by", 1) => {
            let mut pairs = sorted_pairs(&args[0], input, vars)?;
            pairs.dedup_by(|later, earlier| cmp_values(&later.0, &earlier.0) == Ordering::Equal);
            one(Value::Array(pairs.into_iter().map(|(_, v)| v).collect()))
        }
        ("first", 1) => Ok(eval(&args[0], input, vars)?.into_iter().take(1).collect()),
        ("limit", 2) => {
            let mut out = Vec::new();
            for n in eval(&args[0], input, vars)? {
                let n = n.as_f64().ok_or("limit requires a number")?.max(0.0) as usize;
                out.extend(eval(&args[1], input, vars)?.into_iter().take(n));
            }
            Ok(out)
        }
        _ => Err(format!("{name}/{} is not defined", args.len())),
    }
}
