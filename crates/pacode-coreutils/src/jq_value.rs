//! Value helpers for `jq`: numbers, truthiness, type names and jq's total order.

use std::cmp::Ordering;

use serde_json::Value;

/// A JSON number from an `f64`; integral values stay integers.
pub(crate) fn num(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 {
        Value::from(f as i64)
    } else {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

pub(crate) fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

pub(crate) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Number(_) => 3,
        Value::String(_) => 4,
        Value::Array(_) => 5,
        Value::Object(_) => 6,
    }
}

/// jq's total order: null < false < true < numbers < strings < arrays < objects.
pub(crate) fn cmp_values(a: &Value, b: &Value) -> Ordering {
    let by_rank = rank(a).cmp(&rank(b));
    if by_rank != Ordering::Equal {
        return by_rank;
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&y.as_f64().unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Array(x), Value::Array(y)) => {
            for (l, r) in x.iter().zip(y.iter()) {
                let ord = cmp_values(l, r);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut xk: Vec<&String> = x.keys().collect();
            let mut yk: Vec<&String> = y.keys().collect();
            xk.sort();
            yk.sort();
            let key_order = xk.cmp(&yk);
            if key_order != Ordering::Equal {
                return key_order;
            }
            for k in xk {
                if let (Some(l), Some(r)) = (x.get(k), y.get(k)) {
                    let ord = cmp_values(l, r);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
            }
            Ordering::Equal
        }
        _ => Ordering::Equal,
    }
}
