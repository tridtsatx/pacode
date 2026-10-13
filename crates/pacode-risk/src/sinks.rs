//! Two argument-aware extractions the gate needs: the inline source a non-shell
//! interpreter runs from the command line (`python -c`, `node -e`, `deno
//! eval`), and the destination a non-`rm` writer opens (`cp`/`mv`/`install`
//! target, `ln` operands, `tee` files, `rsync` destination, `sponge` file).
//!
//! These only pull the relevant words out; the gate assesses them (inline code
//! as a risk tier, a destination through the protected-path checks).

use crate::tokenize::Token;

/// Interpreters that run inline source from a flag, with the flags that carry
/// it. `deno` is handled separately (its `eval` subcommand).
const INLINE_CODE: &[(&str, &[&str])] = &[
    ("python", &["-c"]),
    ("python2", &["-c"]),
    ("python3", &["-c"]),
    ("node", &["-e", "--eval", "-p", "--print"]),
    ("nodejs", &["-e", "--eval", "-p", "--print"]),
    ("bun", &["-e", "--eval"]),
    ("perl", &["-e", "-E"]),
    ("ruby", &["-e"]),
    ("php", &["-r"]),
    ("lua", &["-e"]),
    ("luajit", &["-e"]),
    ("Rscript", &["-e"]),
];

/// Obvious destructive payloads in inline interpreter source. Not a parser —
/// a few literal markers that lift a benign-looking tier to confirmation.
const DESTRUCTIVE_MARKERS: &[&str] = &[
    "rm -rf",
    "os.system",
    "subprocess",
    "child_process",
    "exec(",
    "system(",
];

/// The inline source strings a non-shell interpreter runs. Empty when the
/// command runs a script file (`python app.py`) or carries no inline code.
pub(crate) fn inline_code<'a>(name: &str, words: &'a [Token]) -> Vec<&'a str> {
    if name == "deno" {
        return deno_eval(words);
    }
    let Some((_, flags)) = INLINE_CODE.iter().find(|(n, _)| *n == name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut index = 1;
    while let Some(word) = words.get(index) {
        index += 1;
        let text = word.text.as_str();
        if let Some(code) = attached_code(text, flags) {
            out.push(code);
        } else if flags.contains(&text)
            && let Some(next) = words.get(index)
        {
            out.push(next.text.as_str());
            index += 1;
        }
    }
    out
}

/// Code attached to its flag: `-eCODE`, `--eval=CODE`.
fn attached_code<'a>(text: &'a str, flags: &[&str]) -> Option<&'a str> {
    for flag in flags {
        if flag.starts_with("--") {
            if let Some(code) = text.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
                return Some(code);
            }
        } else if let Some(code) = text.strip_prefix(flag).filter(|c| !c.is_empty()) {
            return Some(code);
        }
    }
    None
}

/// `deno eval [opts] CODE` — the first operand after the `eval` subcommand.
fn deno_eval(words: &[Token]) -> Vec<&str> {
    let mut index = 1;
    while let Some(word) = words.get(index) {
        if word.is_flag() {
            index += 1;
            continue;
        }
        if word.text == "eval" {
            return words[index + 1..]
                .iter()
                .find(|w| !w.is_flag())
                .map(|w| vec![w.text.as_str()])
                .unwrap_or_default();
        }
        break;
    }
    Vec::new()
}

/// Whether inline source contains an obvious destructive payload.
pub(crate) fn code_is_destructive(code: &str) -> bool {
    DESTRUCTIVE_MARKERS.iter().any(|m| code.contains(m))
}

/// The paths a non-`rm` writing command opens for writing. `cp`/`mv`/`install`
/// write their `-t DIR` or last operand; `ln` both its target and its link
/// name; `tee` and `sponge` their file operands; `rsync` its last operand.
pub(crate) fn write_destinations<'a>(name: &str, words: &'a [Token]) -> Vec<&'a str> {
    match name {
        "cp" | "mv" | "install" => target_dir_or_last(words),
        "rsync" => operands(words).last().copied().into_iter().collect(),
        "ln" | "tee" | "sponge" => operands(words),
        _ => Vec::new(),
    }
}

/// `-t DIR` / `--target-directory[=DIR]` when present, otherwise the last
/// operand (the destination of `cp SRC... DEST`).
fn target_dir_or_last(words: &[Token]) -> Vec<&str> {
    let mut index = 1;
    let mut last = None;
    while let Some(word) = words.get(index) {
        index += 1;
        let text = word.text.as_str();
        if matches!(text, "-t" | "--target-directory")
            && let Some(dir) = words.get(index)
        {
            return vec![dir.text.as_str()];
        }
        if let Some(dir) = text.strip_prefix("--target-directory=") {
            return vec![dir];
        }
        if let Some(dir) = text.strip_prefix("-t").filter(|d| !d.is_empty()) {
            return vec![dir];
        }
        if !word.is_flag() {
            last = Some(text);
        }
    }
    last.into_iter().collect()
}

fn operands(words: &[Token]) -> Vec<&str> {
    words
        .iter()
        .skip(1)
        .filter(|w| !w.is_flag())
        .map(|w| w.text.as_str())
        .collect()
}

#[cfg(test)]
#[path = "sinks_tests.rs"]
mod sinks_tests;
