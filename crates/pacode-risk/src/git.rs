//! `git [global options] <subcommand> [args]`: where the subcommand is.
//!
//! Global options come before the subcommand and some take a value (`-C
//! <path>`, `-c <name>=<value>`), so "the first word that is not a flag" is not
//! the subcommand: in `git -C log commit -am x` it is `commit`.

/// Global options that take a value, attached with `=` or as the next word.
const VALUE_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
    "--list-cmds",
];

/// Global options without a value (`--exec-path` takes an optional `=path`).
const FLAG_OPTIONS: &[&str] = &[
    "-p",
    "-P",
    "--paginate",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--no-lazy-fetch",
    "--no-optional-locks",
    "--no-advice",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--exec-path",
    "--html-path",
    "--man-path",
    "--info-path",
    "--version",
    "-v",
    "--help",
    "-h",
];

/// A parsed `git` invocation (the words after `git`).
#[derive(Debug, Default)]
pub(crate) struct GitCall<'a> {
    /// `(option, value)` for each global option, values included.
    pub(crate) globals: Vec<(&'a str, Option<&'a str>)>,
    pub(crate) subcommand: Option<&'a str>,
    /// The words after the subcommand.
    pub(crate) args: Vec<&'a str>,
    /// A global option this table does not know.
    pub(crate) unknown: Option<&'a str>,
}

impl<'a> GitCall<'a> {
    pub(crate) fn parse(words: &[&'a str]) -> Self {
        let mut call = GitCall::default();
        let mut index = 0;
        while let Some(&word) = words.get(index) {
            index += 1;
            if !word.starts_with('-') {
                call.subcommand = Some(word);
                call.args = words[index..].to_vec();
                return call;
            }
            let (name, attached) = match word.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value)),
                _ => (word, None),
            };
            if VALUE_OPTIONS.contains(&name) {
                let value = attached.or_else(|| {
                    index += 1;
                    words.get(index - 1).copied()
                });
                call.globals.push((name, value));
            } else if FLAG_OPTIONS.contains(&name) {
                call.globals.push((name, attached));
            } else {
                // `-C<path>` / `-c<name=value>` attached forms.
                match (word.get(..2), word.get(2..)) {
                    (Some(short @ ("-C" | "-c")), Some(value)) if !value.is_empty() => {
                        call.globals.push((short, Some(value)));
                    }
                    _ => {
                        call.unknown = Some(word);
                        return call;
                    }
                }
            }
        }
        call
    }

    /// Values of `-c name=value` overrides.
    pub(crate) fn config_overrides(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.globals
            .iter()
            .filter(|(option, _)| *option == "-c")
            .filter_map(|(_, value)| *value)
    }

    /// Whether the call is only `--version` / `--help`.
    pub(crate) fn only_asks_version(&self) -> bool {
        self.subcommand.is_none()
            && self.unknown.is_none()
            && self
                .globals
                .iter()
                .any(|(option, _)| matches!(*option, "--version" | "-v" | "--help" | "-h"))
    }
}

/// `-c` keys whose value git runs as a shell command line, and the value's
/// command line. An alias runs only when it starts with `!`.
pub(crate) fn command_in_config(setting: &str) -> Option<&str> {
    let (key, value) = setting.split_once('=')?;
    let key = key.to_ascii_lowercase();
    if key.starts_with("alias.") {
        return value.strip_prefix('!');
    }
    let runs = [
        "pager",
        "editor",
        "external",
        "sshcommand",
        "fsmonitor",
        "textconv",
        "command",
        "program",
        "helper",
        "askpass",
        "hookspath",
        "clean",
        "smudge",
        "process",
        "packobjectshook",
    ];
    let last = key.rsplit('.').next().unwrap_or(&key);
    (runs.contains(&last) || key.starts_with("pager.")).then_some(value)
}
