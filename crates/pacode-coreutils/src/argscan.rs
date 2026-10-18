//! Best-effort extraction of the file operands of a utility invocation, so callers can gate
//! reads outside the workspace. Over-approximating is safe (it only asks more often).

/// Short options of `name` that consume the following argument.
fn value_short(name: &str) -> &'static [char] {
    match name {
        "head" | "tail" => &['n', 'c'],
        "cut" => &['d', 'f', 'c'],
        "sort" => &['k', 't'],
        "nl" => &['b', 'n', 'w', 's', 'v', 'i'],
        "paste" => &['d'],
        "join" => &['t', '1', '2'],
        "du" => &['d'],
        "tree" => &['L'],
        "diff" => &['U'],
        "base64" => &['w'],
        "xxd" => &['l', 's', 'c'],
        "stat" => &['c'],
        "sed" => &['e'],
        _ => &[],
    }
}

/// Long options that consume the following argument when written without `=`.
const VALUE_LONG: &[&str] = &[
    "lines",
    "bytes",
    "delimiter",
    "fields",
    "characters",
    "key",
    "field-separator",
    "wrap",
    "max-depth",
    "format",
    "level",
    "suffix",
    "delimiters",
    "expression",
    "number-format",
    "number-width",
    "body-numbering",
    "number-separator",
    "starting-line-number",
    "line-increment",
    "unified",
    "output-delimiter",
    "len",
    "seek",
    "cols",
];

/// Operands of `name args...` that may name files or directories.
pub fn path_operands(name: &str, args: &[String]) -> Vec<String> {
    if matches!(name, "basename" | "dirname" | "tr") {
        return Vec::new();
    }
    let script_first = name == "jq"
        || (name == "sed"
            && !args
                .iter()
                .any(|a| a == "-e" || a.starts_with("--expression")));
    let mut out = Vec::new();
    let mut skip = 0usize;
    let mut after_dashdash = false;
    let mut script_skipped = false;
    for arg in args {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if !after_dashdash && arg == "--" {
            after_dashdash = true;
            continue;
        }
        if !after_dashdash && arg.starts_with('-') && arg.len() > 1 {
            if let Some(long) = arg.strip_prefix("--") {
                if long == "arg" || long == "argjson" {
                    skip = 2;
                } else if !long.contains('=') && VALUE_LONG.contains(&long) {
                    skip = 1;
                }
            } else {
                let mut chars = arg.chars().skip(1);
                if let (Some(c), None) = (chars.next(), chars.next())
                    && value_short(name).contains(&c)
                {
                    skip = 1;
                }
            }
            continue;
        }
        if script_first && !script_skipped {
            script_skipped = true;
            continue;
        }
        if arg != "-" {
            out.push(arg.clone());
        }
    }
    out
}

#[cfg(test)]
#[path = "argscan_tests.rs"]
mod argscan_tests;
