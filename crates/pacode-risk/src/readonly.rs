//! Read-only allowlist for Plan mode.
//!
//! The allowlist answers "does this command only read?" for every command the
//! line runs ([`crate::shell`] finds them, wrappers included), so it has to
//! survive commands that read *and* write or execute through their own
//! arguments: `fd -x`, `rg --pre`, `sort -o`, `uniq IN OUT`, `find -fprintf`,
//! `sed w`, `awk 'print > "f"'`, `git diff --output=` (see [`crate::writers`]).
//! A substitution, a writing redirection, or grammar the parser cannot follow
//! makes the line not read-only.
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

use crate::git::GitCall;
use crate::paths::{ProtectedPaths, is_safe_redirect_sink};
use crate::shell::{self, Script, SimpleCommand, expand_braces};
use crate::tokenize::Token;
use crate::wrappers::{self, Inner};
use crate::writers::{awk_writes, sed_writes, writes_through_own_arguments};

const READONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
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

/// Wrappers that change only scheduling, limits or the environment, so the
/// command they run may still be a read.
const READ_WRAPPERS: &[&str] = &[
    "env", "nice", "ionice", "time", "timeout", "nohup", "command", "builtin", "exec", "xargs",
    "stdbuf", "setsid",
];

/// System password databases: readable by anyone, a dump of every account's
/// hash, and never a legitimate thing to pull into a model's context.
const SYSTEM_SECRET_FILES: &[&str] = &["/etc/shadow", "/etc/gshadow", "/etc/master.passwd"];

/// Brace patterns expanded for the path checks before giving up.
const MAX_BRACE_WORDS: usize = 64;

pub fn is_read_only(command: &str) -> bool {
    read_only_script(&shell::parse(command))
}

pub(crate) fn read_only_script(script: &Script) -> bool {
    !script.commands.is_empty()
        && script.unresolved.is_empty()
        && script.substitutions == 0
        && script.commands.iter().all(command_is_read_only)
}

fn command_is_read_only(command: &SimpleCommand) -> bool {
    let redirects_read = command.redirects.iter().all(|redirect| {
        let Some(target) = &redirect.target else {
            return true;
        };
        if redirect.writes() {
            is_safe_redirect_sink(&target.text)
        } else {
            !escapes(&target.text)
        }
    });
    redirects_read
        && !command
            .assignments
            .iter()
            .any(|a| assignment_runs_code(&a.text))
        && (command.words.is_empty() || words_are_read_only(&command.words))
}

/// An environment assignment that makes a reader run other code: the search
/// path, preloaded libraries, shell start-up files, pagers and editors, git's
/// external helpers.
fn assignment_runs_code(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    matches!(
        name,
        "PATH"
            | "IFS"
            | "BASH_ENV"
            | "ENV"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "PS4"
            | "PROMPT_COMMAND"
            | "LESSOPEN"
            | "LESSCLOSE"
            | "VISUAL"
            | "NODE_OPTIONS"
            | "GIT_EXTERNAL_DIFF"
            | "GIT_SSH"
            | "GIT_SSH_COMMAND"
            | "GIT_ASKPASS"
            | "SSH_ASKPASS"
            | "GIT_EXEC_PATH"
    ) || name.starts_with("LD_")
        || name.starts_with("DYLD_")
        || name.starts_with("GIT_CONFIG")
        || name.contains("PAGER")
        || name.contains("EDITOR")
}

fn words_are_read_only(words: &[Token]) -> bool {
    if let Some(unwrapped) = wrappers::unwrap(words) {
        let name = unwrapped.name.as_str();
        if !READ_WRAPPERS.contains(&name) || !unwrapped.writes.is_empty() {
            return false;
        }
        if name == "env" && words.iter().any(|w| assignment_runs_code(&w.text)) {
            return false;
        }
        return match unwrapped.inner {
            Inner::Argv(rest) => words_are_read_only(rest),
            // `env` alone prints the whole environment — every token in it.
            Inner::Nothing => name != "env",
            Inner::Scripts(_) | Inner::Opaque(_) | Inner::Unknown(_) => false,
        };
    }
    let Some(program) = words.first() else {
        return true;
    };
    let program_name = program.basename();
    if reads_escape_workspace(&program_name, words) {
        return false;
    }
    let args: Vec<&str> = words.iter().skip(1).map(|t| t.text.as_str()).collect();
    if READONLY_COMMANDS.contains(&program_name.as_str()) {
        return !writes_through_own_arguments(&program_name, words);
    }
    match program_name.as_str() {
        "find" => !args.iter().any(|a| {
            matches!(
                *a,
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
        "awk" | "gawk" | "mawk" => !awk_writes(&args),
        "sed" => !sed_writes(&args),
        "git" => is_git_read_only(&args),
        "cargo" => is_cargo_read_only(&args),
        "rustc" | "python" | "python3" | "node" | "nodejs" => is_version_only(&args),
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
    tokens.iter().any(|t| {
        !t.is_flag()
            && expansions(&t.text)
                .iter()
                .any(|word| is_protected_read(word))
    }) || path_operands(program, tokens).into_iter().any(climbs_out)
}

/// A redirection's file: protected, or outside the workspace.
fn escapes(target: &str) -> bool {
    expansions(target)
        .iter()
        .any(|word| is_protected_read(word))
        || climbs_out(target)
}

/// `operand`, brace patterns included, has a `..` path component.
fn climbs_out(operand: &str) -> bool {
    expansions(operand)
        .iter()
        .any(|word| word.split('/').any(|part| part == ".."))
}

/// The words a brace pattern expands to; a pattern too large to expand is
/// kept as one word with every `..` it might produce made explicit.
fn expansions(word: &str) -> Vec<String> {
    expand_braces(word, MAX_BRACE_WORDS).unwrap_or_else(|| vec!["..".to_string()])
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

fn is_version_only(args: &[&str]) -> bool {
    !args.is_empty() && args.iter().all(|a| matches!(*a, "--version" | "-V" | "-v"))
}

/// `cargo --version`, and `metadata` / `tree` only when they may not rewrite
/// `Cargo.lock` (`--locked` / `--frozen`). `check` is not a read: it runs
/// build scripts and proc macros (project code) and writes `target/`. A
/// `--config` / `-Z` override can point cargo at any program.
fn is_cargo_read_only(args: &[&str]) -> bool {
    if args
        .iter()
        .any(|a| *a == "--config" || a.starts_with("--config=") || a.starts_with("-Z"))
    {
        return false;
    }
    if args.iter().any(|a| matches!(*a, "--version" | "-V")) {
        return true;
    }
    let Some(subcmd) = args.iter().find(|a| !a.starts_with('-')) else {
        return false;
    };
    matches!(*subcmd, "metadata" | "tree")
        && args.iter().any(|a| matches!(*a, "--locked" | "--frozen"))
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

fn is_git_read_only(words: &[&str]) -> bool {
    let call = GitCall::parse(words);
    if call.unknown.is_some() {
        return false;
    }
    if call.only_asks_version() {
        return true;
    }
    // `-c` / `--config-env` can name any program as a pager, diff driver or
    // alias; `--exec-path` swaps git's own helpers.
    if call
        .globals
        .iter()
        .any(|(option, _)| matches!(*option, "-c" | "--config-env" | "--exec-path"))
    {
        return false;
    }
    let Some(subcmd) = call.subcommand else {
        return false;
    };
    let args = &call.args;
    // `--output=<file>` writes the result to disk (`git diff --output=patch`).
    if args
        .iter()
        .any(|a| a.starts_with("--o") && "--output".starts_with(git_flag(a)))
    {
        return false;
    }
    match subcmd {
        "status" | "log" | "diff" | "show" | "blame" | "rev-parse" | "ls-files" | "describe" => {
            true
        }
        "branch" => is_git_ref_listing(
            args,
            &[
                "-d",
                "-D",
                "-m",
                "-M",
                "--delete",
                "--move",
                "-c",
                "-C",
                "--copy",
                "--edit-description",
                "-u",
                "--set-upstream-to",
                "--unset-upstream",
                "-f",
                "--force",
                "-t",
                "--track",
                "--no-track",
                "--create-reflog",
                "--recurse-submodules",
            ],
        ),
        "tag" => is_git_ref_listing(
            args,
            &[
                "-d",
                "--delete",
                "-a",
                "--annotate",
                "-m",
                "--message",
                "-F",
                "--file",
                "-f",
                "--force",
                "-s",
                "--sign",
                "-u",
                "--local-user",
                "--edit",
                "-e",
                "--create-reflog",
            ],
        ),
        "stash" => matches!(args.first(), Some(&"list" | &"show")),
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
        _ => false,
    }
}

/// `git branch` / `git tag` list refs only without a writing flag, and with
/// a name given only as a `-l`/`--list` pattern or a filter's value:
/// `git branch NAME` creates a branch, `git tag NAME` a tag.
fn is_git_ref_listing(args: &[&str], writing: &[&str]) -> bool {
    const FILTERS: &[&str] = &[
        "--contains",
        "--no-contains",
        "--merged",
        "--no-merged",
        "--points-at",
        "--sort",
        "--format",
    ];
    if args.iter().any(|a| writing.contains(&git_flag(a))) {
        return false;
    }
    let mut listing = false;
    let mut names = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if FILTERS.contains(arg) && args.get(index).is_some_and(|v| !v.starts_with('-')) {
            index += 1;
        } else if matches!(*arg, "-l" | "--list") {
            listing = true;
        } else if !arg.starts_with('-') {
            names = true;
        }
    }
    !names || listing
}
