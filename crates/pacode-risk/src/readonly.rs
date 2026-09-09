//! Read-only allowlist for Plan mode.

#[cfg(test)]
#[path = "readonly_tests.rs"]
mod readonly_tests;

use crate::paths::is_safe_redirect_sink;
use crate::tokenize::{self, Token};

const READONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "wc",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "fd",
    "tree",
    "pwd",
    "echo",
    "printf",
    "which",
    "type",
    "env",
    "printenv",
    "stat",
    "file",
    "du",
    "df",
    "date",
    "uname",
    "whoami",
    "id",
    "ps",
    "jq",
    "sort",
    "uniq",
    "cut",
    "diff",
    "md5sum",
    "sha256sum",
    "true",
    "false",
    "test",
    "[",
    "dirname",
    "basename",
    "realpath",
    "readlink",
];

const SHELL_CONTROL_PREFIXES: &[&str] = &[
    "then", "do", "else", "elif", "if", "while", "until", "case", "in", "select",
];

pub fn is_read_only(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.contains("$(") || trimmed.contains('`') {
        return false;
    }

    let tokens = tokenize::tokenize(trimmed);
    if tokens.is_empty() {
        return false;
    }

    for token in &tokens {
        if token.is_truncating_redirect_target && !is_safe_redirect_sink(&token.text) {
            return false;
        }
        if token.is_operator && (token.text == ">>" || token.text.ends_with(">>")) {
            return false;
        }
    }

    let segments = tokenize::split_segments(trimmed);
    if segments.is_empty() {
        return false;
    }

    for segment in &segments {
        if !is_segment_read_only(segment) {
            return false;
        }
    }

    true
}

fn is_segment_read_only(tokens: &[Token]) -> bool {
    let mut tokens = tokens;
    while tokens.len() > 1
        && tokens
            .first()
            .is_some_and(|t| SHELL_CONTROL_PREFIXES.contains(&t.text.as_str()))
    {
        tokens = &tokens[1..];
    }

    loop {
        let Some(first) = tokens.first() else {
            return false;
        };
        let name = first.basename();

        if name == "sudo" || name == "doas" || name == "tee" {
            return false;
        }

        if name == "xargs" {
            let rest = &tokens[1..];
            let mut idx = 0;
            while idx < rest.len() {
                let token = &rest[idx];
                if token.is_operator {
                    idx += 1;
                    continue;
                }
                if token.is_flag() {
                    idx += 1;
                    if matches!(
                        token.text.as_str(),
                        "-n" | "--max-args"
                            | "-P"
                            | "--max-procs"
                            | "-s"
                            | "--max-chars"
                            | "-d"
                            | "-E"
                            | "-I"
                            | "-L"
                    ) && idx < rest.len()
                    {
                        idx += 1;
                    }
                    continue;
                }
                break;
            }
            let inner = &rest[idx..];
            if inner.is_empty() {
                return true;
            }
            return is_segment_read_only(inner);
        }

        if matches!(
            name.as_str(),
            "env"
                | "nice"
                | "ionice"
                | "time"
                | "timeout"
                | "nohup"
                | "command"
                | "builtin"
                | "exec"
        ) {
            let rest = &tokens[1..];
            let mut idx = 0;
            while idx < rest.len() {
                let token = &rest[idx];
                if token.is_operator || token.text.contains('=') {
                    idx += 1;
                    continue;
                }
                if token.is_flag() {
                    idx += 1;
                    if matches!(
                        token.text.as_str(),
                        "-n" | "--adjustment" | "-c" | "-p" | "-s" | "-k" | "-u"
                    ) && idx < rest.len()
                    {
                        idx += 1;
                    }
                    continue;
                }
                if token.text.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    idx += 1;
                    continue;
                }
                break;
            }
            if idx >= rest.len() {
                return name == "env";
            }
            tokens = &rest[idx..];
            continue;
        }

        break;
    }

    let Some(program) = tokens.first() else {
        return false;
    };
    let program_name = program.basename();

    if READONLY_COMMANDS.contains(&program_name.as_str()) {
        return true;
    }

    match program_name.as_str() {
        "find" => !tokens.iter().any(|t| {
            matches!(
                t.text.as_str(),
                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
            )
        }),
        "awk" | "gawk" | "mawk" => !tokens
            .iter()
            .any(|t| t.text.contains("system(") || t.text.contains("system (")),
        "sed" => !tokens.iter().any(|t| {
            t.text == "-i" || t.text.starts_with("-i") || t.text.starts_with("--in-place")
        }),
        "git" => is_git_read_only(tokens),
        "cargo" => is_cargo_read_only(tokens),
        "rustc" => is_version_only(tokens),
        "python" | "python3" => is_version_only(tokens),
        "node" | "nodejs" => is_version_only(tokens),
        _ => false,
    }
}

fn is_version_only(tokens: &[Token]) -> bool {
    let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
    if args.is_empty() {
        return false;
    }
    args.iter().any(|a| matches!(*a, "--version" | "-V" | "-v"))
        && args.iter().all(|a| matches!(*a, "--version" | "-V" | "-v"))
}

fn is_cargo_read_only(tokens: &[Token]) -> bool {
    let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
    if args.iter().any(|a| matches!(*a, "--version" | "-V")) {
        return true;
    }
    let Some(subcmd) = args.iter().find(|a| !a.starts_with('-')) else {
        return false;
    };
    matches!(*subcmd, "check" | "metadata" | "tree")
}

fn is_git_read_only(tokens: &[Token]) -> bool {
    let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
    if args.iter().any(|a| matches!(*a, "--version" | "-v")) {
        return true;
    }
    let Some(subcmd) = args.iter().find(|a| !a.starts_with('-')) else {
        return false;
    };
    match *subcmd {
        "status" | "log" | "diff" | "show" | "blame" | "rev-parse" | "ls-files" | "describe" => {
            true
        }
        "branch" => !args
            .iter()
            .any(|a| matches!(*a, "-d" | "-D" | "-m" | "-M" | "--delete")),
        "tag" => !args
            .iter()
            .any(|a| matches!(*a, "-d" | "-a" | "-m" | "--delete")),
        "stash" => {
            let stash_sub = args.iter().skip_while(|a| **a != "stash").nth(1).copied();
            matches!(stash_sub, Some("list" | "show"))
        }
        "remote" => !args
            .iter()
            .any(|a| matches!(*a, "add" | "remove" | "rm" | "set-url" | "rename")),
        "list" => true,
        _ => false,
    }
}
