//! Read-only allowlist for Plan mode.
//!
//! The allowlist answers "does this command only read?", so it has to survive
//! commands that read *and* write through their own flags: `fd -x`, `rg
//! --pre`, `sort -o`, `find -fprintf`, `sed w`, `awk 'print > "f"'`, `git
//! diff --output=` all execute or write while looking like a read.
//!
//! Reads are gated too, at the only level that needs no working directory:
//! credential stores and credential-shaped file names (the `paths` policy that
//! protects them from deletion), the system password databases, and any operand
//! that climbs out of the workspace with `..`. An absolute path elsewhere is
//! still readable — the workspace boundary needs the session `cwd`, which this
//! signature does not carry.

#[cfg(test)]
#[path = "readonly_tests.rs"]
mod readonly_tests;

use crate::paths::{ProtectedPaths, is_safe_redirect_sink};
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

/// System password databases: readable by anyone, a dump of every account's
/// hash, and never a legitimate thing to pull into a model's context.
const SYSTEM_SECRET_FILES: &[&str] = &["/etc/shadow", "/etc/gshadow", "/etc/master.passwd"];

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
            // `env` on its own (or with only assignments) prints the whole
            // environment — every token in it — and nothing else.
            if idx >= rest.len() {
                return false;
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

    if reads_escape_workspace(&program_name, tokens) {
        return false;
    }

    if READONLY_COMMANDS.contains(&program_name.as_str()) {
        return !writes_through_own_flags(&program_name, tokens);
    }

    match program_name.as_str() {
        "find" => !tokens.iter().any(|t| {
            matches!(
                t.text.as_str(),
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    // `-fprint`/`-fprintf`/`-fls` write the walk to a file.
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        "awk" | "gawk" | "mawk" => !tokens.iter().any(|t| {
            t.text.contains("system(")
                || t.text.contains("system (")
                || (!t.is_flag() && redirects_print_to_file(&t.text))
        }),
        "sed" => !tokens.iter().skip(1).any(|t| {
            t.text == "-i"
                || t.text.starts_with("-i")
                || t.text.starts_with("--in-place")
                || (!t.is_flag() && sed_writes_file(&t.text))
        }),
        "git" => is_git_read_only(tokens),
        "cargo" => is_cargo_read_only(tokens),
        "rustc" => is_version_only(tokens),
        "python" | "python3" => is_version_only(tokens),
        "node" | "nodejs" => is_version_only(tokens),
        _ => false,
    }
}

/// Allowlisted commands that write or execute anyway when handed a flag, and
/// `printenv` with no variable named.
fn writes_through_own_flags(program: &str, tokens: &[Token]) -> bool {
    let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
    match program {
        // `fd -x/-X/--exec` runs a command per match.
        "fd" => args
            .iter()
            .any(|a| matches!(*a, "-x" | "-X" | "--exec" | "--exec-batch")),
        // `rg --pre` pipes every file it reads through a command.
        "rg" => args.iter().any(|a| a.starts_with("--pre")),
        // `sort -o` rewrites (or truncates) the file it sorts.
        "sort" => args
            .iter()
            .any(|a| *a == "-o" || *a == "--output" || a.starts_with("--output=")),
        // A bare `printenv` dumps every variable, keys included.
        "printenv" => args.is_empty(),
        _ => false,
    }
}

/// Operands of `program` that are read as *paths*. A `..` inside one of them
/// leaves the workspace; inside a pattern (`sed 's/../x/g'`, `find -path
/// '*/../*'`) it is only a regex, so pattern positions are skipped.
fn path_operands<'a>(program: &str, tokens: &'a [Token]) -> Vec<&'a str> {
    let mut operands = Vec::new();
    // The first word after the program of these is a pattern or a script,
    // never a path.
    let mut want_pattern = matches!(program, "grep" | "egrep" | "fgrep" | "rg" | "awk" | "sed");
    let mut flag_takes_pattern = false;
    for token in tokens.iter().skip(1) {
        if token.is_operator {
            continue;
        }
        if token.is_flag() {
            flag_takes_pattern |= matches!(
                token.text.as_str(),
                "-e" | "--regexp"
                    | "--glob"
                    | "--iglob"
                    | "--pre-glob"
                    | "-name"
                    | "-iname"
                    | "-path"
                    | "-wholename"
                    | "-iwholename"
                    | "-regex"
                    | "-iregex"
            );
            continue;
        }
        if flag_takes_pattern || want_pattern {
            flag_takes_pattern = false;
            want_pattern = false;
            continue;
        }
        operands.push(token.text.as_str());
    }
    operands
}

/// Read targets the Plan-mode policy refuses: the credential stores and
/// credential-shaped file names that `paths` protects from deletion, the system
/// password databases, and operands that climb out of the workspace.
fn reads_escape_workspace(program: &str, tokens: &[Token]) -> bool {
    if tokens
        .iter()
        .any(|t| !t.is_operator && !t.is_flag() && is_protected_read(&t.text))
    {
        return true;
    }
    path_operands(program, tokens)
        .into_iter()
        .any(|operand| operand.split('/').any(|part| part == ".."))
}

/// Whether this word names a credential store, a credential file, or a system
/// password database — `~/.ssh/id_rsa`, `./.aws/credentials`, `app/.env`,
/// `server.key`, `/etc/shadow`.
fn is_protected_read(word: &str) -> bool {
    if SYSTEM_SECRET_FILES.contains(&word) {
        return true;
    }
    let parts: Vec<&str> = word
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts
        .iter()
        .any(|part| ProtectedPaths::credential_subpaths().contains(part))
    {
        return true;
    }
    parts
        .last()
        .is_some_and(|name| ProtectedPaths::is_credential_file_name(name))
}

/// `print > "file"` and `print >> "f"` are awk's write path. A comparison
/// (`$3 > 5`) is not, so only an arrow whose right-hand side is a quoted string
/// counts — that is the only form awk accepts for a filename.
fn redirects_print_to_file(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("print") {
        let at = from + found;
        let tail = &lower[at + "print".len()..];
        let Some(arrow) = tail.find('>') else {
            return false;
        };
        let target = tail[arrow + 1..].trim_start_matches('>').trim_start();
        if target.starts_with('"') || target.starts_with('\'') {
            return true;
        }
        from = at + "print".len();
    }
    false
}

/// `w` is sed's write command: `sed 'w out'` and the `s/a/b/w out` tail both
/// create a file while the command still reads like a filter.
fn sed_writes_file(token: &str) -> bool {
    if token.is_empty() || token.starts_with('-') {
        return false;
    }
    let chars: Vec<char> = token.chars().collect();
    let mut idx = 0;
    let mut at_command_start = true;
    while idx < chars.len() {
        let c = chars[idx];
        if c == ';' {
            at_command_start = true;
            idx += 1;
            continue;
        }
        if c.is_whitespace() {
            idx += 1;
            continue;
        }
        // Skip an `s///` command to its closing delimiter, where a `w` tail may
        // follow.
        if c == 's' && at_command_start {
            let Some(delim) = chars.get(idx + 1).copied().filter(|d| !d.is_whitespace()) else {
                at_command_start = false;
                idx += 1;
                continue;
            };
            let mut cursor = idx + 2;
            let mut delimiters = 0;
            while cursor < chars.len() && delimiters < 2 {
                if chars[cursor] == '\\' {
                    cursor += 2;
                    continue;
                }
                if chars[cursor] == delim {
                    delimiters += 1;
                }
                cursor += 1;
            }
            at_command_start = true;
            idx = cursor;
            continue;
        }
        if (c == 'w' || c == 'W') && at_command_start {
            return true;
        }
        at_command_start = false;
        idx += 1;
    }
    false
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

/// `--flag` and `--flag=value` name the same option to git; the value may
/// itself contain `=`, and an argument that is not a flag is never normalized.
fn git_flag(arg: &str) -> &str {
    if arg.starts_with('-') {
        arg.split('=').next().unwrap_or(arg)
    } else {
        arg
    }
}

fn is_git_read_only(tokens: &[Token]) -> bool {
    let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
    if args.iter().any(|a| matches!(*a, "--version" | "-v")) {
        return true;
    }
    // `--output=<file>` writes the result to disk (`git diff --output=patch`).
    if args.iter().any(|a| git_flag(a) == "--output") {
        return false;
    }
    let Some(subcmd) = args.iter().find(|a| !a.starts_with('-')) else {
        return false;
    };
    match *subcmd {
        "status" | "log" | "diff" | "show" | "blame" | "rev-parse" | "ls-files" | "describe" => {
            true
        }
        "branch" => !args.iter().any(|a| {
            matches!(
                git_flag(a),
                "-d" | "-D"
                    | "-m"
                    | "-M"
                    | "--delete"
                    | "--move"
                    | "-c"
                    | "-C"
                    | "--copy"
                    | "--edit-description"
                    | "-u"
                    | "--set-upstream-to"
                    | "--unset-upstream"
            )
        }),
        "tag" => is_git_tag_listing(&args),
        "stash" => {
            let stash_sub = args.iter().skip_while(|a| **a != "stash").nth(1).copied();
            matches!(stash_sub, Some("list" | "show"))
        }
        "remote" => !args.iter().any(|a| {
            matches!(
                *a,
                "add"
                    | "remove"
                    | "rm"
                    | "set-url"
                    | "rename"
                    | "update"
                    | "prune"
                    | "set-head"
                    | "set-branches"
            )
        }),
        "list" => true,
        _ => false,
    }
}

/// `git tag <name>` creates a tag and moves a ref; only the listing forms read.
fn is_git_tag_listing(args: &[&str]) -> bool {
    if args.iter().any(|a| {
        matches!(
            git_flag(a),
            "-d" | "--delete"
                | "-a"
                | "--annotate"
                | "-m"
                | "--message"
                | "-f"
                | "--force"
                | "-s"
                | "--sign"
                | "-u"
                | "--local-user"
                | "--edit"
                | "-e"
                | "--create-reflog"
        )
    }) {
        return false;
    }
    if args.iter().any(|a| matches!(git_flag(a), "-l" | "--list")) {
        return true;
    }
    !args.iter().skip(1).any(|a| !a.starts_with('-'))
}
