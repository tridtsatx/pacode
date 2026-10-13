//! Commands that may run side by side with the other calls of a model step.
//!
//! Stricter than [`crate::readonly`] on purpose. Plan mode asks "does this leave
//! the project as it was?", and for script interpreters (`sed`, `awk`), runners
//! (`xargs`, `env`) and tool versions it answers by reading their arguments —
//! a heuristic that is acceptable there because a miss is still a command the
//! user's mode allows. Running two calls at once asks more: nothing either one
//! does may be seen by, or wait on, the other, and a miss is silent — a write
//! racing a read, a build lock held twice, project code (a `build.rs`, a
//! `node --require`) running beside the next call. So only plain readers with
//! fixed behaviour qualify: they read files or repository state and print.
//! No interpreters, no build tools, nothing that executes project code, and the
//! line must also pass every read-only check.

use crate::readonly::read_only_script;
use crate::shell;
use crate::tokenize::Token;
use crate::wrappers::{self, Inner};

/// Readers whose only effect is their output. `git` is listed for its
/// read-only subcommands, which `read_only_script` alone admits.
const PURE_READERS: &[&str] = &[
    "cat",
    "head",
    "tail",
    "wc",
    "ls",
    "stat",
    "file",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "fd",
    "find",
    "tree",
    "du",
    "df",
    "pwd",
    "echo",
    "printf",
    "which",
    "type",
    "printenv",
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
    "git",
];

/// Wrappers that only bound or time the reader they run.
const PURE_WRAPPERS: &[&str] = &["timeout", "nice", "time"];

/// Whether `command` only reads, with nothing another concurrent call could
/// observe or race with.
pub fn is_side_effect_free(command: &str) -> bool {
    let script = shell::parse(command);
    read_only_script(&script)
        && script
            .commands
            .iter()
            .all(|c| c.assignments.is_empty() && pure_words(&c.words))
}

fn pure_words(words: &[Token]) -> bool {
    let Some(program) = words.first() else {
        return true;
    };
    if let Some(unwrapped) = wrappers::unwrap(words) {
        return PURE_WRAPPERS.contains(&unwrapped.name.as_str())
            && matches!(unwrapped.inner, Inner::Argv(rest) if pure_words(rest));
    }
    PURE_READERS.contains(&program.basename().as_str())
}
