//! Commands that run another command: `sudo`, `env`, `timeout`, `xargs`, `eval`, ...
//!
//! Finding the command behind a wrapper means knowing which of its options take
//! a value: `env -u NAME rm` runs `rm`, not `NAME`; `xargs -a FILE rm` reads
//! `rm`'s arguments from `FILE`. Each wrapper's options are listed here, from
//! its getopt string, and an option the table does not know makes the command
//! unknown instead of guessed. The risk gate and the read-only check both
//! unwrap through this one table.

use crate::tokenize::Token;

/// What a wrapper runs.
#[derive(Debug)]
pub(crate) enum Inner<'a> {
    /// These words, as a command.
    Argv(&'a [Token]),
    /// Each string as a shell command line.
    Scripts(Vec<String>),
    /// Nothing (`command -v x`, `env` alone, an interactive `su`).
    Nothing,
    /// A runner whose options this table does not model: any of these words
    /// may start the command it runs.
    Opaque(&'a [Token]),
    /// Cannot tell; the reason.
    Unknown(String),
}

#[derive(Debug)]
pub(crate) struct Unwrapped<'a> {
    /// The wrapper's basename.
    pub(crate) name: String,
    pub(crate) inner: Inner<'a>,
    /// Files the wrapper itself writes (`time -o FILE`).
    pub(crate) writes: Vec<Token>,
}

/// One wrapper's option grammar.
struct Spec {
    /// Short options taking a value, attached (`-uNAME`) or next (`-u NAME`).
    short_values: &'static str,
    /// Short options whose value can only be attached (`-i{}`).
    short_optional: &'static str,
    short_flags: &'static str,
    /// Long options taking a value (`--user=NAME`, `--user NAME`).
    long_values: &'static [&'static str],
    /// Long options without one, or with an attached optional one (`--eof=X`).
    long_flags: &'static [&'static str],
    /// Operands before the command (`timeout DURATION`, `chroot NEWROOT`).
    operands: usize,
    /// `-N` is an option (`nice -10`).
    numeric: bool,
}

const NO_OPTIONS: Spec = Spec {
    short_values: "",
    short_optional: "",
    short_flags: "",
    long_values: &[],
    long_flags: &[],
    operands: 0,
    numeric: false,
};

/// Shells that run their arguments as command lines (`sh -c '...'`).
pub(crate) const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "yash", "fish", "rbash", "tcsh", "csh",
];

/// Runners with option grammars too varied to table; every word may start
/// the command they run.
const OPAQUE: &[&str] = &[
    "strace",
    "ltrace",
    "taskset",
    "chrt",
    "prlimit",
    "unshare",
    "nsenter",
    "firejail",
    "systemd-run",
    "runuser",
    "pkexec",
    "unbuffer",
    "flock",
    "valgrind",
    "catchsegv",
    "faketime",
    "script",
    "sg",
    "parallel",
];

fn spec(name: &str) -> Option<Spec> {
    Some(match name {
        "sudo" => Spec {
            short_values: "aCcDgpRrTtUu",
            short_flags: "ABbEeHhiKklNnPSsVv",
            long_values: &[
                "auth-type",
                "close-from",
                "chdir",
                "group",
                "host",
                "login-class",
                "prompt",
                "chroot",
                "role",
                "type",
                "command-timeout",
                "other-user",
                "user",
            ],
            long_flags: &[
                "askpass",
                "background",
                "bell",
                "preserve-env",
                "edit",
                "set-home",
                "login",
                "remove-timestamp",
                "reset-timestamp",
                "list",
                "non-interactive",
                "preserve-groups",
                "stdin",
                "shell",
                "validate",
            ],
            ..NO_OPTIONS
        },
        "doas" => Spec {
            short_values: "Cu",
            short_flags: "Lns",
            ..NO_OPTIONS
        },
        "env" => Spec {
            short_values: "CSu",
            short_flags: "iv0",
            long_values: &["chdir", "split-string", "unset"],
            long_flags: &[
                "ignore-environment",
                "null",
                "debug",
                "block-signal",
                "default-signal",
                "ignore-signal",
                "list-signal-handling",
            ],
            ..NO_OPTIONS
        },
        "nice" => Spec {
            short_values: "n",
            long_values: &["adjustment"],
            numeric: true,
            ..NO_OPTIONS
        },
        "ionice" => Spec {
            short_values: "cnpPu",
            short_flags: "t",
            long_values: &["class", "classdata", "pid", "pgid", "uid"],
            long_flags: &["ignore"],
            ..NO_OPTIONS
        },
        "timeout" => Spec {
            short_values: "ks",
            short_flags: "v",
            long_values: &["kill-after", "signal"],
            long_flags: &["preserve-status", "foreground", "verbose"],
            operands: 1,
            ..NO_OPTIONS
        },
        "setsid" => Spec {
            short_flags: "cfw",
            long_flags: &["ctty", "fork", "wait"],
            ..NO_OPTIONS
        },
        "stdbuf" => Spec {
            short_values: "ioe",
            long_values: &["input", "output", "error"],
            ..NO_OPTIONS
        },
        "chroot" => Spec {
            long_values: &["userspec", "groups"],
            long_flags: &["skip-chdir"],
            operands: 1,
            ..NO_OPTIONS
        },
        "time" => Spec {
            short_values: "fo",
            short_flags: "apqvV",
            long_values: &["format", "output"],
            long_flags: &["append", "portability", "quiet", "verbose"],
            ..NO_OPTIONS
        },
        "exec" => Spec {
            short_values: "a",
            short_flags: "cl",
            ..NO_OPTIONS
        },
        "command" => Spec {
            short_flags: "pvV",
            ..NO_OPTIONS
        },
        "xargs" => Spec {
            short_values: "adEILnPs",
            short_optional: "eil",
            short_flags: "0oprtx",
            long_values: &[
                "arg-file",
                "delimiter",
                "max-args",
                "max-procs",
                "max-chars",
                "process-slot-var",
            ],
            long_flags: &[
                "null",
                "eof",
                "replace",
                "max-lines",
                "open-tty",
                "interactive",
                "no-run-if-empty",
                "verbose",
                "exit",
                "show-limits",
            ],
            ..NO_OPTIONS
        },
        "watch" => Spec {
            short_values: "n",
            short_flags: "bcdegptxwqr",
            long_values: &["interval"],
            long_flags: &[
                "beep",
                "color",
                "no-color",
                "differences",
                "errexit",
                "chgexit",
                "precise",
                "no-title",
                "exec",
                "no-wrap",
            ],
            ..NO_OPTIONS
        },
        "trap" => Spec {
            short_flags: "lp",
            ..NO_OPTIONS
        },
        "nohup" | "builtin" | "eval" | "busybox" | "toybox" => NO_OPTIONS,
        _ => return None,
    })
}

/// A short option bundle or long option the spec does not know.
fn unknown(name: &str, option: &str) -> Inner<'static> {
    Inner::Unknown(format!(
        "`{name}` is given option `{option}`, which is not known here, so the \
         command it runs cannot be identified"
    ))
}

/// Options parsed off the front of a wrapper's words.
struct Options<'a> {
    /// Index of the first operand.
    rest: usize,
    /// `(option, value)` for every option that took a value.
    values: Vec<(String, Option<&'a str>)>,
}

/// Parse `spec`'s options at the start of `words` (stopping at the first
/// operand, as the `+` getopt strings of these tools do).
fn options<'a>(spec: &Spec, words: &'a [Token]) -> Result<Options<'a>, String> {
    let mut values = Vec::new();
    let mut index = 0;
    while let Some(word) = words.get(index) {
        let text = word.text.as_str();
        index += 1;
        if text == "--" {
            break;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            if spec.long_values.contains(&name) {
                let value = match attached {
                    Some(value) => Some(value),
                    None => {
                        index += 1;
                        words.get(index - 1).map(|t| t.text.as_str())
                    }
                };
                values.push((name.to_string(), value));
            } else if spec.long_flags.contains(&name) {
                values.push((name.to_string(), attached));
            } else {
                return Err(text.to_string());
            }
            continue;
        }
        let Some(bundle) = text.strip_prefix('-').filter(|b| !b.is_empty()) else {
            index -= 1;
            break;
        };
        if spec.numeric && bundle.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        for (offset, flag) in bundle.char_indices() {
            let attached = &bundle[offset + flag.len_utf8()..];
            if spec.short_values.contains(flag) {
                let value = if attached.is_empty() {
                    index += 1;
                    words.get(index - 1).map(|t| t.text.as_str())
                } else {
                    Some(attached)
                };
                values.push((flag.to_string(), value));
                break;
            }
            if spec.short_optional.contains(flag) {
                values.push((flag.to_string(), Some(attached)));
                break;
            }
            if !spec.short_flags.contains(flag) {
                return Err(text.to_string());
            }
            values.push((flag.to_string(), None));
        }
    }
    Ok(Options {
        rest: index.min(words.len()),
        values,
    })
}

/// `words[0]` names a wrapper: what it runs.
pub(crate) fn unwrap(words: &[Token]) -> Option<Unwrapped<'_>> {
    let first = words.first()?;
    let name = first.basename();
    let args = &words[1..];
    let mut writes = Vec::new();
    let inner = if SHELLS.contains(&name.as_str()) {
        Inner::Scripts(
            args.iter()
                .filter(|t| !t.is_flag())
                .map(|t| t.text.clone())
                .collect(),
        )
    } else if OPAQUE.contains(&name.as_str()) {
        Inner::Opaque(args)
    } else if name == "su" {
        su(args)
    } else {
        let spec = spec(&name)?;
        match options(&spec, args) {
            Err(option) => unknown(&name, &option),
            Ok(parsed) => {
                let mut rest = &args[parsed.rest..];
                if name == "time" {
                    writes.extend(
                        parsed
                            .values
                            .iter()
                            .filter(|(option, _)| option == "o" || option == "output")
                            .filter_map(|(_, file)| file.map(Token::word)),
                    );
                }
                if name == "env" {
                    let assignments = rest
                        .iter()
                        .take_while(|t| t.text == "-" || crate::shell::is_assignment(&t.text))
                        .count();
                    rest = &rest[assignments..];
                }
                let skip = spec.operands.min(rest.len());
                rest = &rest[skip..];
                inner_of(&name, &parsed.values, rest)
            }
        }
    };
    Some(Unwrapped {
        name,
        inner,
        writes,
    })
}

fn joined(words: &[Token]) -> String {
    words
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn inner_of<'a>(name: &str, values: &[(String, Option<&str>)], rest: &'a [Token]) -> Inner<'a> {
    let has = |option: &str| values.iter().any(|(o, _)| o == option);
    match name {
        "eval" | "watch" if rest.is_empty() => Inner::Nothing,
        "eval" | "watch" => Inner::Scripts(vec![joined(rest)]),
        "trap" => match rest.first().map(|t| t.text.as_str()) {
            None | Some("-" | "-p" | "-l" | "--") => Inner::Nothing,
            Some(action) => Inner::Scripts(vec![action.to_string()]),
        },
        "env" => {
            let split = values
                .iter()
                .find(|(o, _)| o == "S" || o == "split-string")
                .and_then(|(_, v)| *v);
            match split {
                Some(script) => Inner::Scripts(vec![format!("{script} {}", joined(rest))]),
                None if rest.is_empty() => Inner::Nothing,
                None => Inner::Argv(rest),
            }
        }
        "command" if has("v") || has("V") => Inner::Nothing,
        _ if rest.is_empty() => Inner::Nothing,
        _ => Inner::Argv(rest),
    }
}

/// `su` permutes its options, and its `-c` command line may come after the
/// user name: every `-c` / `--command` anywhere is the command it runs.
fn su(args: &[Token]) -> Inner<'_> {
    let mut scripts = Vec::new();
    let mut index = 0;
    while let Some(word) = args.get(index) {
        let text = word.text.as_str();
        index += 1;
        if let Some(value) = text
            .strip_prefix("--command=")
            .or_else(|| text.strip_prefix("--session-command="))
        {
            scripts.push(value.to_string());
        } else if matches!(text, "--command" | "--session-command") {
            scripts.extend(args.get(index).map(|t| t.text.clone()));
            index += 1;
        } else if let Some(bundle) = text.strip_prefix('-').filter(|b| !b.starts_with('-'))
            && let Some(at) = bundle.find('c')
        {
            let attached = &bundle[at + 1..];
            if attached.is_empty() {
                scripts.extend(args.get(index).map(|t| t.text.clone()));
                index += 1;
            } else {
                scripts.push(attached.to_string());
            }
        }
    }
    if scripts.is_empty() {
        Inner::Nothing
    } else {
        Inner::Scripts(scripts)
    }
}

#[cfg(test)]
#[path = "wrappers_tests.rs"]
mod wrappers_tests;
