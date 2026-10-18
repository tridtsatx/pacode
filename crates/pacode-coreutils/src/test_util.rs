//! Shared helpers for unit tests.

use std::path::Path;

use crate::UtilOutput;

/// Run `name` with string-slice args and optional stdin text in a throwaway cwd.
pub(crate) fn run_in(cwd: &Path, name: &str, args: &[&str], stdin: Option<&str>) -> UtilOutput {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    crate::run(name, &args, stdin.map(str::as_bytes), cwd)
}

/// Run and return `(stdout, code)`.
pub(crate) fn sh(name: &str, args: &[&str], stdin: Option<&str>) -> (String, i32) {
    let out = run_in(Path::new("."), name, args, stdin);
    (out.stdout_lossy(), out.code)
}

/// Run and return stdout, asserting exit 0.
pub(crate) fn ok(name: &str, args: &[&str], stdin: &str) -> String {
    let out = run_in(Path::new("."), name, args, Some(stdin));
    assert_eq!(out.code, 0, "{name} {args:?}: {}", out.stderr);
    out.stdout_lossy()
}
