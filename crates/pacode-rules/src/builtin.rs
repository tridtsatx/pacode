//! Built-in rule packs, embedded at compile time. To add a language: drop the
//! rule files under `builtin/<lang>/` and list them in [`PACKS`]. Users can also
//! add languages at runtime through `.pacode/rules/*.md` without touching this.

use crate::parse::parse_rule;
use crate::rule::{Origin, Rule, RuleError};

pub struct Pack {
    pub name: &'static str,
    /// `(file name, file content)`.
    pub files: &'static [(&'static str, &'static str)],
}

pub const PACKS: &[Pack] = &[
    Pack {
        name: "rust",
        files: &[
            (
                "rust/no-unwrap-expect.md",
                include_str!("../builtin/rust/no-unwrap-expect.md"),
            ),
            (
                "rust/unsafe-needs-safety.md",
                include_str!("../builtin/rust/unsafe-needs-safety.md"),
            ),
            (
                "rust/no-todo.md",
                include_str!("../builtin/rust/no-todo.md"),
            ),
            ("rust/no-dbg.md", include_str!("../builtin/rust/no-dbg.md")),
            (
                "rust/no-println.md",
                include_str!("../builtin/rust/no-println.md"),
            ),
            (
                "rust/no-lossy-cast.md",
                include_str!("../builtin/rust/no-lossy-cast.md"),
            ),
            (
                "rust/no-box-leak.md",
                include_str!("../builtin/rust/no-box-leak.md"),
            ),
            (
                "rust/allow-needs-reason.md",
                include_str!("../builtin/rust/allow-needs-reason.md"),
            ),
            (
                "rust/no-transmute.md",
                include_str!("../builtin/rust/no-transmute.md"),
            ),
        ],
    },
    Pack {
        name: "typescript",
        files: &[
            (
                "typescript/no-explicit-any.md",
                include_str!("../builtin/typescript/no-explicit-any.md"),
            ),
            (
                "typescript/no-ts-ignore.md",
                include_str!("../builtin/typescript/no-ts-ignore.md"),
            ),
            (
                "typescript/no-console-log.md",
                include_str!("../builtin/typescript/no-console-log.md"),
            ),
            (
                "typescript/no-var.md",
                include_str!("../builtin/typescript/no-var.md"),
            ),
            (
                "typescript/strict-equality.md",
                include_str!("../builtin/typescript/strict-equality.md"),
            ),
            (
                "typescript/prefer-union-over-enum.md",
                include_str!("../builtin/typescript/prefer-union-over-enum.md"),
            ),
        ],
    },
];

/// Parse every embedded rule. A malformed built-in is reported, not fatal (the
/// unit tests assert there are none).
pub fn builtin_rules() -> (Vec<Rule>, Vec<RuleError>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for pack in PACKS {
        for &(file, source) in pack.files {
            match parse_rule(source, Origin::Builtin(file)) {
                Ok(rule) => rules.push(rule),
                Err(e) => errors.push(e),
            }
        }
    }
    (rules, errors)
}
