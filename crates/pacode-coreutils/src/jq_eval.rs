//! Evaluator and builtins for the supported `jq` subset.
//!
//! Every filter produces a `Vec` of outputs; results are capped at [`MAX_RESULTS`].

use std::cmp::Ordering;
use std::collections::HashMap;

use regex::RegexBuilder;
use serde_json::{Map, Value};

use crate::jq_parse::{Ast, BinOp};

pub(crate) const MAX_RESULTS: usize = 200_000;
const MAX_REPEAT_BYTES: usize = 1 << 20;

pub(crate) type Vars = HashMap<String, Value>;
pub(crate) type Res = Result<Vec<Value>, String>;

pub(crate) use crate::jq_value::{cmp_values, num, truthy, type_name};

fn check(out: Vec<Value>) -> Res {
    if out.len() > MAX_RESULTS {
        Err(format!("too many results (limit {MAX_RESULTS})"))
    } else {
        Ok(out)
    }
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => compact(other),
    }
}

fn get_field(v: &Value, name: &str) -> Result<Value, String> {
    match v {
        Value::Object(m) => Ok(m.get(name).cloned().unwrap_or(Value::Null)),
        Value::Null => Ok(Value::Null),
        other => Err(format!("Cannot index {} with \"{name}\"", type_name(other))),
    }
}

fn index_value(b: &Value, i: &Value) -> Result<Value, String> {
    match (b, i) {
        (Value::Object(m), Value::String(k)) => Ok(m.get(k).cloned().unwrap_or(Value::Null)),
        (Value::Array(a), Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0).floor();
            let idx = if f < 0.0 { a.len() as f64 + f } else { f };
            if idx < 0.0 {
                return Ok(Value::Null);
            }
            Ok(a.get(idx as usize).cloned().unwrap_or(Value::Null))
        }
        (Value::Null, Value::String(_) | Value::Number(_) | Value::Null) => Ok(Value::Null),
        (b, i) => Err(format!(
            "Cannot index {} with {}",
            type_name(b),
            type_name(i)
        )),
    }
}

fn slice_bounds(len: usize, from: &Value, to: &Value) -> Result<(usize, usize), String> {
    let bad = || "Start and end indices of an array slice must be numbers".to_string();
    let f = match from {
        Value::Null => 0.0,
        v => v.as_f64().ok_or_else(bad)?,
    };
    let t = match to {
        Value::Null => len as f64,
        v => v.as_f64().ok_or_else(bad)?,
    };
    let l = len as f64;
    let norm = |x: f64| -> f64 {
        let x = if x < 0.0 { l + x } else { x };
        x.max(0.0).min(l)
    };
    let s = norm(f.floor()) as usize;
    let e = norm(t.ceil()) as usize;
    Ok((s, e.max(s)))
}

fn slice_value(b: &Value, from: &Value, to: &Value) -> Result<Value, String> {
    match b {
        Value::Array(a) => {
            let (s, e) = slice_bounds(a.len(), from, to)?;
            Ok(Value::Array(a[s..e].to_vec()))
        }
        Value::String(text) => {
            let chars: Vec<char> = text.chars().collect();
            let (s, e) = slice_bounds(chars.len(), from, to)?;
            Ok(Value::String(chars[s..e].iter().collect()))
        }
        Value::Null => Ok(Value::Null),
        other => Err(format!("Cannot index {} with object", type_name(other))),
    }
}

fn iterate(v: &Value) -> Res {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(m) => Ok(m.values().cloned().collect()),
        other => Err(format!("Cannot iterate over {}", type_name(other))),
    }
}

fn nums(a: &Value, b: &Value) -> Option<(f64, f64)> {
    Some((a.as_f64()?, b.as_f64()?))
}

fn arith(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    match op {
        BinOp::Add => {
            if let Some((x, y)) = nums(a, b) {
                return Ok(num(x + y));
            }
            match (a, b) {
                (Value::Null, x) | (x, Value::Null) => Ok(x.clone()),
                (Value::String(x), Value::String(y)) => Ok(Value::String(format!("{x}{y}"))),
                (Value::Array(x), Value::Array(y)) => {
                    let mut v = x.clone();
                    v.extend(y.iter().cloned());
                    Ok(Value::Array(v))
                }
                (Value::Object(x), Value::Object(y)) => {
                    let mut m = x.clone();
                    for (k, v) in y {
                        m.insert(k.clone(), v.clone());
                    }
                    Ok(Value::Object(m))
                }
                _ => Err(format!(
                    "{} and {} cannot be added",
                    type_name(a),
                    type_name(b)
                )),
            }
        }
        BinOp::Sub => {
            if let Some((x, y)) = nums(a, b) {
                return Ok(num(x - y));
            }
            match (a, b) {
                (Value::Array(x), Value::Array(y)) => Ok(Value::Array(
                    x.iter()
                        .filter(|e| !y.iter().any(|o| cmp_values(e, o) == Ordering::Equal))
                        .cloned()
                        .collect(),
                )),
                _ => Err(format!(
                    "{} and {} cannot be subtracted",
                    type_name(a),
                    type_name(b)
                )),
            }
        }
        BinOp::Mul => {
            if let Some((x, y)) = nums(a, b) {
                return Ok(num(x * y));
            }
            match (a, b) {
                (Value::String(s), n) | (n, Value::String(s)) if n.is_number() => {
                    let count = n.as_f64().unwrap_or(0.0);
                    if count <= 0.0 {
                        return Ok(Value::Null);
                    }
                    let count = count.ceil() as usize;
                    if s.len().saturating_mul(count) > MAX_REPEAT_BYTES {
                        return Err("string repetition is too large".to_string());
                    }
                    Ok(Value::String(s.repeat(count)))
                }
                _ => Err(format!(
                    "{} and {} cannot be multiplied",
                    type_name(a),
                    type_name(b)
                )),
            }
        }
        BinOp::Div => {
            if let Some((x, y)) = nums(a, b) {
                if y == 0.0 {
                    return Err(
                        "number and number cannot be divided because the divisor is zero"
                            .to_string(),
                    );
                }
                return Ok(num(x / y));
            }
            match (a, b) {
                (Value::String(s), Value::String(sep)) => Ok(split(s, sep)),
                _ => Err(format!(
                    "{} and {} cannot be divided",
                    type_name(a),
                    type_name(b)
                )),
            }
        }
        BinOp::Mod => match nums(a, b) {
            Some((x, y)) => {
                let (xi, yi) = (x as i64, y as i64);
                if yi == 0 {
                    return Err(
                        "number and number cannot be divided because the divisor is zero"
                            .to_string(),
                    );
                }
                Ok(num(xi.wrapping_rem(yi) as f64))
            }
            None => Err(format!(
                "{} and {} cannot be divided",
                type_name(a),
                type_name(b)
            )),
        },
        BinOp::Eq => Ok(Value::Bool(cmp_values(a, b) == Ordering::Equal)),
        BinOp::Ne => Ok(Value::Bool(cmp_values(a, b) != Ordering::Equal)),
        BinOp::Lt => Ok(Value::Bool(cmp_values(a, b) == Ordering::Less)),
        BinOp::Le => Ok(Value::Bool(cmp_values(a, b) != Ordering::Greater)),
        BinOp::Gt => Ok(Value::Bool(cmp_values(a, b) == Ordering::Greater)),
        BinOp::Ge => Ok(Value::Bool(cmp_values(a, b) != Ordering::Less)),
        BinOp::And | BinOp::Or | BinOp::Alt => {
            Err("internal: logical operator in arith".to_string())
        }
    }
}

fn split(text: &str, sep: &str) -> Value {
    if text.is_empty() {
        return Value::Array(Vec::new());
    }
    if sep.is_empty() {
        return Value::Array(text.chars().map(|c| Value::String(c.to_string())).collect());
    }
    Value::Array(
        text.split(sep)
            .map(|p| Value::String(p.to_string()))
            .collect(),
    )
}

fn binary(op: BinOp, l: &Ast, r: &Ast, input: &Value, vars: &Vars) -> Res {
    let mut out = Vec::new();
    match op {
        BinOp::Alt => {
            let found: Vec<Value> = eval(l, input, vars)
                .unwrap_or_default()
                .into_iter()
                .filter(truthy)
                .collect();
            if found.is_empty() {
                return eval(r, input, vars);
            }
            return Ok(found);
        }
        BinOp::And => {
            for lv in eval(l, input, vars)? {
                if !truthy(&lv) {
                    out.push(Value::Bool(false));
                } else {
                    for rv in eval(r, input, vars)? {
                        out.push(Value::Bool(truthy(&rv)));
                    }
                }
            }
        }
        BinOp::Or => {
            for lv in eval(l, input, vars)? {
                if truthy(&lv) {
                    out.push(Value::Bool(true));
                } else {
                    for rv in eval(r, input, vars)? {
                        out.push(Value::Bool(truthy(&rv)));
                    }
                }
            }
        }
        _ => {
            let rs = eval(r, input, vars)?;
            let ls = eval(l, input, vars)?;
            for rv in &rs {
                for lv in &ls {
                    out.push(arith(op, lv, rv)?);
                }
            }
        }
    }
    check(out)
}

fn object(entries: &[(Ast, Ast)], input: &Value, vars: &Vars) -> Res {
    let mut acc: Vec<Map<String, Value>> = vec![Map::new()];
    for (k, v) in entries {
        let keys = eval(k, input, vars)?;
        let vals = eval(v, input, vars)?;
        let mut next = Vec::new();
        for m in &acc {
            for key in &keys {
                let Value::String(name) = key else {
                    return Err(format!(
                        "Object keys must be strings, not {}",
                        type_name(key)
                    ));
                };
                for val in &vals {
                    let mut copy = m.clone();
                    copy.insert(name.clone(), val.clone());
                    next.push(copy);
                    if next.len() > MAX_RESULTS {
                        return Err(format!("too many results (limit {MAX_RESULTS})"));
                    }
                }
            }
        }
        acc = next;
    }
    Ok(acc.into_iter().map(Value::Object).collect())
}

fn first_output(arg: Option<&Ast>, input: &Value, vars: &Vars) -> Result<Value, String> {
    match arg {
        None => Ok(Value::Null),
        Some(a) => Ok(eval(a, input, vars)?
            .into_iter()
            .next()
            .unwrap_or(Value::Null)),
    }
}

/// Evaluate `ast` against `input`.
pub(crate) fn eval(ast: &Ast, input: &Value, vars: &Vars) -> Res {
    match ast {
        Ast::Identity => Ok(vec![input.clone()]),
        Ast::Literal(v) => Ok(vec![v.clone()]),
        Ast::Var(name) => vars
            .get(name)
            .map(|v| vec![v.clone()])
            .ok_or_else(|| format!("${name} is not defined")),
        Ast::Field(base, name) => {
            let mut out = Vec::new();
            for b in eval(base, input, vars)? {
                out.push(get_field(&b, name)?);
            }
            Ok(out)
        }
        Ast::Index(base, idx) => {
            let idxs = eval(idx, input, vars)?;
            let mut out = Vec::new();
            for b in eval(base, input, vars)? {
                for i in &idxs {
                    out.push(index_value(&b, i)?);
                }
            }
            check(out)
        }
        Ast::Slice(base, from, to) => {
            let f = first_output(from.as_deref(), input, vars)?;
            let t = first_output(to.as_deref(), input, vars)?;
            let mut out = Vec::new();
            for b in eval(base, input, vars)? {
                out.push(slice_value(&b, &f, &t)?);
            }
            Ok(out)
        }
        Ast::Iterate(base) => {
            let mut out = Vec::new();
            for b in eval(base, input, vars)? {
                out.extend(iterate(&b)?);
                if out.len() > MAX_RESULTS {
                    return check(out);
                }
            }
            Ok(out)
        }
        Ast::Try(inner) => Ok(eval(inner, input, vars).unwrap_or_default()),
        Ast::Pipe(a, b) => {
            let mut out = Vec::new();
            for x in eval(a, input, vars)? {
                out.extend(eval(b, &x, vars)?);
                if out.len() > MAX_RESULTS {
                    return check(out);
                }
            }
            Ok(out)
        }
        Ast::Comma(a, b) => {
            let mut out = eval(a, input, vars)?;
            out.extend(eval(b, input, vars)?);
            check(out)
        }
        Ast::Neg(a) => {
            let mut out = Vec::new();
            for v in eval(a, input, vars)? {
                match v.as_f64() {
                    Some(f) => out.push(num(-f)),
                    None => return Err(format!("{} cannot be negated", type_name(&v))),
                }
            }
            Ok(out)
        }
        Ast::Binary(op, l, r) => binary(*op, l, r, input, vars),
        Ast::If(cond, then, otherwise) => {
            let mut out = Vec::new();
            for c in eval(cond, input, vars)? {
                if truthy(&c) {
                    out.extend(eval(then, input, vars)?);
                } else if let Some(e) = otherwise {
                    out.extend(eval(e, input, vars)?);
                } else {
                    out.push(input.clone());
                }
            }
            check(out)
        }
        Ast::Array(None) => Ok(vec![Value::Array(Vec::new())]),
        Ast::Array(Some(inner)) => Ok(vec![Value::Array(eval(inner, input, vars)?)]),
        Ast::Object(entries) => object(entries, input, vars),
        Ast::Call(name, args) => crate::jq_builtins::call(name, args, input, vars),
    }
}

/// Regex test used by `test/1,2`.
pub(crate) fn regex_test(text: &str, pattern: &str, flags: &str) -> Result<bool, String> {
    let re = RegexBuilder::new(pattern)
        .case_insensitive(flags.contains('i'))
        .build()
        .map_err(|e| format!("invalid regex '{pattern}': {e}"))?;
    Ok(re.is_match(text))
}

pub(crate) fn text_of(v: &Value) -> String {
    to_text(v)
}

pub(crate) fn compact_json(v: &Value) -> String {
    compact(v)
}

pub(crate) fn split_text(text: &str, sep: &str) -> Value {
    split(text, sep)
}

pub(crate) fn arith_add(a: &Value, b: &Value) -> Result<Value, String> {
    arith(BinOp::Add, a, b)
}

pub(crate) fn iterate_values(v: &Value) -> Res {
    iterate(v)
}
