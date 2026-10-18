//! Tiny declarative option parser shared by all utilities.
//!
//! Unknown options are errors: nothing is ever silently ignored.

use crate::UtilError;

/// One accepted option.
pub(crate) struct Flag {
    pub id: &'static str,
    /// `'\0'` when there is no short form.
    pub short: char,
    /// `""` when there is no long form.
    pub long: &'static str,
    pub takes_value: bool,
}

/// A switch without a value.
pub(crate) const fn sw(id: &'static str, short: char, long: &'static str) -> Flag {
    Flag {
        id,
        short,
        long,
        takes_value: false,
    }
}

/// An option that takes a value.
pub(crate) const fn val(id: &'static str, short: char, long: &'static str) -> Flag {
    Flag {
        id,
        short,
        long,
        takes_value: true,
    }
}

#[derive(Default)]
pub(crate) struct Parsed {
    opts: Vec<(&'static str, Option<String>)>,
    pub operands: Vec<String>,
}

impl Parsed {
    pub fn has(&self, id: &str) -> bool {
        self.opts.iter().any(|(i, _)| *i == id)
    }

    /// Value of the last occurrence of `id`.
    pub fn get(&self, id: &str) -> Option<&str> {
        self.opts
            .iter()
            .rev()
            .find(|(i, _)| *i == id)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Values of every occurrence of `id`, in order.
    pub fn all(&self, id: &str) -> Vec<&str> {
        self.opts
            .iter()
            .filter(|(i, _)| *i == id)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }
}

/// Parse `args` against `spec`. With `numeric`, `-5` becomes option `num` = "5".
pub(crate) fn parse(args: &[String], spec: &[Flag], numeric: bool) -> Result<Parsed, UtilError> {
    let mut parsed = Parsed::default();
    let mut only_operands = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if only_operands || arg == "-" || !arg.starts_with('-') {
            parsed.operands.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_operands = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            let flag = spec
                .iter()
                .find(|f| !f.long.is_empty() && f.long == name)
                .ok_or_else(|| UtilError::usage(format!("unsupported option --{name}")))?;
            if flag.takes_value {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        let v = args.get(i).ok_or_else(|| {
                            UtilError::usage(format!("option --{name} requires a value"))
                        })?;
                        i += 1;
                        v.clone()
                    }
                };
                parsed.opts.push((flag.id, Some(value)));
            } else {
                if inline.is_some() {
                    return Err(UtilError::usage(format!(
                        "option --{name} does not take a value"
                    )));
                }
                parsed.opts.push((flag.id, None));
            }
            continue;
        }
        let body = &arg[1..];
        if numeric && body.chars().all(|c| c.is_ascii_digit()) {
            parsed.opts.push(("num", Some(body.to_string())));
            continue;
        }
        let chars: Vec<char> = body.chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            j += 1;
            let flag = spec
                .iter()
                .find(|f| f.short != '\0' && f.short == c)
                .ok_or_else(|| UtilError::usage(format!("unsupported option -{c}")))?;
            if flag.takes_value {
                let rest: String = chars[j..].iter().collect();
                let value = if rest.is_empty() {
                    let v = args
                        .get(i)
                        .ok_or_else(|| UtilError::usage(format!("option -{c} requires a value")))?;
                    i += 1;
                    v.clone()
                } else {
                    rest
                };
                parsed.opts.push((flag.id, Some(value)));
                break;
            }
            parsed.opts.push((flag.id, None));
        }
    }
    Ok(parsed)
}

/// Parse a non-negative integer option value.
pub(crate) fn parse_usize(what: &str, text: &str) -> Result<usize, UtilError> {
    text.trim()
        .parse::<usize>()
        .map_err(|_| UtilError::usage(format!("invalid {what}: '{text}'")))
}
