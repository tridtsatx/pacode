//! In-process implementations of the text utilities an agent reaches for most often.
//!
//! Pure library: no async runtime, no processes. Every utility has the shape
//! `fn(args, stdin, cwd) -> UtilResult` and [`run`] dispatches by name and folds errors into a
//! [`UtilOutput`] (usage errors and unsupported flags exit 2, runtime errors exit 1).
//! Flags are never silently ignored: an option a utility does not implement is an error.
//! Inputs larger than [`MAX_INPUT_BYTES`] are refused and output is capped at
//! [`MAX_OUTPUT_BYTES`].

use std::path::Path;

mod args;
pub mod argscan;
pub mod cut;
pub mod diff;
pub mod fsinfo;
pub mod hash;
mod io;
pub mod jq;
mod jq_builtins;
mod jq_eval;
mod jq_lex;
mod jq_parse;
mod jq_value;
pub mod lines;
pub mod md5;
pub mod merge;
pub mod pipeline;
pub mod sed;
pub mod sorting;
pub mod tr;
pub mod tree;
pub mod xxd;

#[cfg(test)]
mod test_util;

/// Largest single input (file or piped data) a utility will read.
pub const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;
/// Largest stdout a utility may produce; the rest is dropped with a note on stderr.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Names of every supported utility.
pub const UTILS: &[&str] = &[
    "wc",
    "head",
    "tail",
    "sort",
    "uniq",
    "cut",
    "tr",
    "nl",
    "tac",
    "rev",
    "paste",
    "join",
    "basename",
    "dirname",
    "realpath",
    "stat",
    "du",
    "tree",
    "diff",
    "jq",
    "sha256sum",
    "md5sum",
    "base64",
    "xxd",
    "sed",
];

/// Result of one utility run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UtilOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub code: i32,
}

impl UtilOutput {
    /// Successful output (exit 0).
    pub fn ok(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            code: 0,
        }
    }

    /// Output with an explicit exit code (e.g. `diff` exits 1 when files differ).
    pub fn with_code(stdout: impl Into<Vec<u8>>, code: i32) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            code,
        }
    }

    /// A failure with no stdout.
    pub fn error(code: i32, message: impl Into<String>) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: message.into(),
            code,
        }
    }

    /// Stdout decoded lossily.
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Why a utility could not complete.
#[derive(Debug, thiserror::Error)]
pub enum UtilError {
    /// Bad usage or an unsupported flag (exit 2).
    #[error("{0}")]
    Usage(String),
    /// A runtime failure such as a missing file (exit 1).
    #[error("{0}")]
    Failed(String),
}

impl UtilError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

pub type UtilResult = Result<UtilOutput, UtilError>;

/// Whether `name` is one of [`UTILS`].
pub fn is_supported(name: &str) -> bool {
    UTILS.contains(&name)
}

/// Run utility `name`. Unknown names exit 2.
pub fn run(name: &str, args: &[String], stdin: Option<&[u8]>, cwd: &Path) -> UtilOutput {
    let result = match name {
        "wc" => lines::wc(args, stdin, cwd),
        "head" => lines::head(args, stdin, cwd),
        "tail" => lines::tail(args, stdin, cwd),
        "tac" => lines::tac(args, stdin, cwd),
        "rev" => lines::rev(args, stdin, cwd),
        "nl" => lines::nl(args, stdin, cwd),
        "sort" => sorting::sort(args, stdin, cwd),
        "uniq" => sorting::uniq(args, stdin, cwd),
        "cut" => cut::cut(args, stdin, cwd),
        "tr" => tr::tr(args, stdin, cwd),
        "paste" => merge::paste(args, stdin, cwd),
        "join" => merge::join(args, stdin, cwd),
        "basename" => fsinfo::basename(args, stdin, cwd),
        "dirname" => fsinfo::dirname(args, stdin, cwd),
        "realpath" => fsinfo::realpath(args, stdin, cwd),
        "stat" => fsinfo::stat(args, stdin, cwd),
        "du" => fsinfo::du(args, stdin, cwd),
        "tree" => tree::tree(args, stdin, cwd),
        "diff" => diff::diff(args, stdin, cwd),
        "jq" => jq::jq(args, stdin, cwd),
        "sha256sum" => hash::sha256sum(args, stdin, cwd),
        "md5sum" => hash::md5sum(args, stdin, cwd),
        "base64" => hash::base64(args, stdin, cwd),
        "xxd" => xxd::xxd(args, stdin, cwd),
        "sed" => sed::sed(args, stdin, cwd),
        other => Err(UtilError::usage(format!("unsupported utility `{other}`"))),
    };
    finish(name, result)
}

fn finish(name: &str, result: UtilResult) -> UtilOutput {
    let mut out = match result {
        Ok(out) => out,
        Err(UtilError::Usage(message)) => UtilOutput::error(2, format!("{name}: {message}")),
        Err(UtilError::Failed(message)) => UtilOutput::error(1, format!("{name}: {message}")),
    };
    if out.stdout.len() > MAX_OUTPUT_BYTES {
        out.stdout.truncate(MAX_OUTPUT_BYTES);
        if !out.stderr.is_empty() && !out.stderr.ends_with('\n') {
            out.stderr.push('\n');
        }
        out.stderr.push_str(&format!(
            "{name}: output truncated at {MAX_OUTPUT_BYTES} bytes\n"
        ));
    }
    out
}
