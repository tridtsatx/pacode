//! Blast-radius assessment of a parsed command line.
//!
//! [`crate::shell`] lists every simple command the line runs and
//! [`crate::wrappers`] finds the command behind `sudo`, `env`, `xargs`, `sh -c`
//! and the rest; this module judges each one: destructive programs and their
//! targets, truncating redirections, known tools with dangerous subcommands.
//! What cannot be read statically — a computed program name, an option a
//! wrapper table does not know, grammar the parser cannot follow — is a
//! `Confirm` finding, never `Safe`.

use std::path::Path;

use crate::git::{GitCall, command_in_config};
use crate::paths::{self, is_safe_redirect_sink};
use crate::shell::{self, SimpleCommand, expand_braces};
use crate::tokenize::Token;
use crate::wrappers::{self, Inner, SHELLS};
use crate::{RiskFinding, RiskLevel};

/// Commands that destroy data as their primary purpose.
const DESTRUCTIVE_COMMANDS: &[&str] = &[
    "rm", "rmdir", "shred", "unlink", "truncate", "dd", "mkfs", "fdisk", "parted", "wipefs", "srm",
];

/// Commands destructive only with specific flags.
const CONDITIONALLY_DESTRUCTIVE: &[(&str, &[&str])] = &[
    ("find", &["-delete", "-exec", "-execdir", "-ok", "-okdir"]),
    ("git", &["clean"]),
    ("chmod", &["-R", "--recursive"]),
    ("chown", &["-R", "--recursive"]),
];

/// Wrappers that run their command with someone else's privileges.
const ELEVATING: &[&str] = &["sudo", "doas", "su", "pkexec", "runuser"];

/// Inline shells (`sh -c "sh -c '...'"`) are followed this deep.
const MAX_DEPTH: usize = 8;

/// Words a brace expansion may produce before the target set counts as unknown.
const MAX_BRACE_WORDS: usize = 64;

/// Suffixes of an opaque runner's words tried as its command.
const MAX_OPAQUE_STARTS: usize = 16;

/// How a command was reached.
#[derive(Clone, Copy, Default)]
struct Via<'a> {
    elevated: bool,
    /// `xargs`, `find -exec`: the command's arguments are produced at runtime.
    fed_by: Option<&'a str>,
}

pub(crate) struct Assessor<'a> {
    cwd: &'a Path,
    home: &'a Path,
    pub(crate) findings: Vec<RiskFinding>,
}

fn confirm(reason: String) -> RiskFinding {
    RiskFinding {
        level: RiskLevel::Confirm,
        reason,
        target: None,
    }
}

/// A program word whose name the shell computes: an expansion, a glob, a
/// brace pattern left after expansion, zsh's `=cmd`.
fn is_computed(word: &Token) -> bool {
    let name = word.basename();
    let glob_class = name
        .find('[')
        .is_some_and(|open| name[open..].contains(']'));
    word.text.starts_with('=')
        || name.contains(['$', '`', '*', '?', '{'])
        || (glob_class && name != "[" && name != "[[")
}

impl<'a> Assessor<'a> {
    pub(crate) fn new(cwd: &'a Path, home: &'a Path) -> Self {
        Self {
            cwd,
            home,
            findings: Vec::new(),
        }
    }

    pub(crate) fn script(&mut self, source: &str, elevated: bool, depth: usize) {
        if depth > MAX_DEPTH {
            self.findings.push(confirm(
                "inline shells are nested too deeply to check".into(),
            ));
            return;
        }
        let script = shell::parse(source);
        let mut unresolved = script.unresolved.clone();
        unresolved.dedup();
        for reason in unresolved {
            self.findings.push(confirm(format!(
                "the command line contains {reason}, which cannot be checked statically"
            )));
        }
        self.download_to_shell(&script.commands);
        let via = Via {
            elevated,
            fed_by: None,
        };
        for command in &script.commands {
            self.command(command, via, depth);
        }
    }

    /// `curl ... | sh`: a remote script executed directly.
    fn download_to_shell(&mut self, commands: &[SimpleCommand]) {
        for pair in commands.windows(2) {
            let (Some(fetch), Some(shell)) = (program(&pair[0].words), program(&pair[1].words))
            else {
                continue;
            };
            if matches!(fetch.as_str(), "curl" | "wget" | "fetch")
                && pair[1].receives_pipe
                && SHELLS.contains(&shell.as_str())
            {
                self.findings.push(confirm(format!(
                    "`{fetch} | {shell}` downloads and executes a remote script directly in the shell"
                )));
            }
        }
    }

    fn command(&mut self, command: &SimpleCommand, via: Via<'_>, depth: usize) {
        for redirect in &command.redirects {
            if let Some(target) = &redirect.target
                && redirect.truncates()
            {
                self.redirect_target(target);
            }
        }
        self.words(&command.words, command.receives_pipe, via, depth);
    }

    fn redirect_target(&mut self, target: &Token) {
        if is_safe_redirect_sink(&target.text) {
            return;
        }
        if let Some(finding) =
            paths::classify_target(&target.text, false, self.cwd, self.home, true)
        {
            self.findings.push(finding);
        }
    }

    fn words(&mut self, words: &[Token], receives_pipe: bool, via: Via<'_>, depth: usize) {
        if words.is_empty() {
            return;
        }
        if let Some(expanded) = self.expanded(words) {
            return self.words(&expanded, receives_pipe, via, depth);
        }
        if let Some(unwrapped) = wrappers::unwrap(words) {
            let name = unwrapped.name.as_str();
            let inner_via = Via {
                elevated: via.elevated || ELEVATING.contains(&name),
                fed_by: if name == "xargs" {
                    Some("xargs")
                } else {
                    via.fed_by
                },
            };
            for file in &unwrapped.writes {
                self.redirect_target(file);
            }
            match unwrapped.inner {
                Inner::Argv(rest) => self.words(rest, receives_pipe, inner_via, depth),
                Inner::Scripts(scripts) => {
                    let inline = words[1..]
                        .iter()
                        .any(|w| w.is_flag() && !w.text.starts_with("--") && w.text.contains('c'));
                    if receives_pipe && SHELLS.contains(&name) && !inline {
                        self.findings.push(confirm(format!(
                            "`{name}` runs the commands it reads from a pipe, which cannot \
                             be checked before they run"
                        )));
                    }
                    for script in scripts {
                        self.script(&script, inner_via.elevated, depth + 1);
                    }
                }
                // `sudo` / `su` with no command open a shell of their own.
                Inner::Nothing if ELEVATING.contains(&name) => {
                    self.findings.push(confirm(format!(
                        "`{name}` runs another command that could not be identified statically"
                    )))
                }
                Inner::Nothing => {}
                Inner::Opaque(rest) => {
                    for start in (0..rest.len()).take(MAX_OPAQUE_STARTS) {
                        if !rest[start].is_flag() {
                            self.words(&rest[start..], receives_pipe, inner_via, depth);
                        }
                    }
                }
                Inner::Unknown(reason) => self.findings.push(confirm(reason)),
            }
            return;
        }
        let program = &words[0];
        if is_computed(program) {
            self.findings.push(RiskFinding {
                level: RiskLevel::Confirm,
                reason: "runs a program whose name is computed at runtime, so what it \
                         does cannot be checked in advance"
                    .to_string(),
                target: Some(program.text.clone()),
            });
            return;
        }
        let name = program.basename();
        self.known_tool(&name, words, depth);
        self.runners(&name, words, depth);
        self.destructive(&name, words, receives_pipe, via);
    }

    /// `words` with every brace pattern expanded, or `None` when nothing
    /// expands. Too many words is a finding of its own.
    fn expanded(&mut self, words: &[Token]) -> Option<Vec<Token>> {
        let mut out = Vec::new();
        let mut changed = false;
        for word in words {
            match expand_braces(&word.text, MAX_BRACE_WORDS) {
                Some(parts) if parts.len() == 1 && parts[0] == word.text => out.push(word.clone()),
                Some(parts) => {
                    changed = true;
                    out.extend(parts.into_iter().map(Token::word));
                }
                None => {
                    self.findings.push(confirm(format!(
                        "`{}` expands to too many words to check",
                        word.text
                    )));
                    return Some(Vec::new());
                }
            }
        }
        (changed && out.len() <= MAX_BRACE_WORDS).then_some(out)
    }

    /// Commands a tool runs from its own arguments: `find -exec`, `fd -x`,
    /// and command lines in `git -c` settings.
    fn runners(&mut self, name: &str, words: &[Token], depth: usize) {
        let fed = Via {
            elevated: false,
            fed_by: Some(if name == "fd" { "fd -x" } else { "find -exec" }),
        };
        match name {
            "find" => {
                for (index, word) in words.iter().enumerate() {
                    if matches!(word.text.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                        self.words(until_terminator(&words[index + 1..]), false, fed, depth);
                    }
                }
            }
            "fd" | "fdfind" => {
                for (index, word) in words.iter().enumerate() {
                    let text = word.text.as_str();
                    let short_exec = text.starts_with('-')
                        && !text.starts_with("--")
                        && text.ends_with(['x', 'X']);
                    if short_exec || matches!(text, "--exec" | "--exec-batch") {
                        self.words(until_terminator(&words[index + 1..]), false, fed, depth);
                    }
                }
            }
            "git" => {
                let texts: Vec<&str> = words[1..].iter().map(|t| t.text.as_str()).collect();
                let call = GitCall::parse(&texts);
                for setting in call.config_overrides() {
                    if let Some(command) = command_in_config(setting) {
                        self.script(command, false, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    fn destructive(&mut self, name: &str, words: &[Token], receives_pipe: bool, via: Via<'_>) {
        let triggered = if DESTRUCTIVE_COMMANDS.contains(&name) || name.starts_with("mkfs.") {
            true
        } else if let Some((_, flags)) = CONDITIONALLY_DESTRUCTIVE.iter().find(|(n, _)| *n == name)
        {
            words.iter().any(|t| flags.contains(&t.text.as_str()))
        } else {
            false
        };
        if !triggered {
            return;
        }
        if let Some(feeder) = via.fed_by {
            let reason = if feeder == "xargs" {
                format!("`xargs {name}` executes destructive operations from standard input")
            } else {
                format!("`{feeder} {name}` runs on paths found at runtime")
            };
            self.findings.push(confirm(reason));
        }
        if via.elevated {
            self.findings.push(confirm(format!(
                "`sudo` runs destructive command `{name}` with elevated privileges"
            )));
        }
        if receives_pipe {
            self.findings.push(confirm(format!(
                "`{name}` deletes paths supplied by a pipe, so the set of \
                 affected files cannot be checked before it runs"
            )));
        }
        let targets: Vec<&Token> = words.iter().skip(1).filter(|t| !t.is_flag()).collect();
        if targets.is_empty() {
            self.findings.push(confirm(format!(
                "`{name}` is destructive but its target could not be \
                 determined statically, so its blast radius is unknown"
            )));
            return;
        }
        let recursive = words.iter().any(Token::is_recursive_flag);
        for target in targets {
            let raw = target
                .text
                .split_once('=')
                .filter(|(key, _)| matches!(*key, "of" | "if" | "seek" | "conv"))
                .map(|(_, value)| value)
                .unwrap_or(&target.text);
            if let Some(finding) =
                paths::classify_target(raw, recursive, self.cwd, self.home, false)
            {
                self.findings.push(finding);
            }
        }
    }

    fn known_tool(&mut self, program: &str, tokens: &[Token], depth: usize) {
        let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
        match program {
            "git" => self.git(&args, depth),
            "cargo" if args.contains(&"clean") => self.findings.push(RiskFinding {
                level: RiskLevel::Low,
                reason: "`cargo clean` deletes all build artifacts".to_string(),
                target: None,
            }),
            "docker" | "podman" if args.contains(&"prune") => self.findings.push(confirm(format!(
                "`{program} prune` removes unused containers, images, or volumes"
            ))),
            "kill" => {
                let kills_all = args.contains(&"-1");
                let is_force = args
                    .iter()
                    .any(|a| matches!(*a, "-9" | "-KILL" | "-s" | "KILL" | "9"));
                if kills_all && is_force {
                    self.findings.push(RiskFinding {
                        level: RiskLevel::Confirm,
                        reason: "`kill -9 -1` terminates all processes accessible to the user"
                            .to_string(),
                        target: Some("-1".to_string()),
                    });
                }
            }
            _ => {}
        }
    }

    fn git(&mut self, words: &[&str], _depth: usize) {
        let call = GitCall::parse(words);
        if let Some(option) = call.unknown {
            self.findings.push(confirm(format!(
                "`git` is given option `{option}`, which is not known here, so the \
                 subcommand cannot be identified"
            )));
            return;
        }
        let Some(subcmd) = call.subcommand else {
            return;
        };
        let args = &call.args;
        if subcmd == "push"
            && args
                .iter()
                .any(|a| matches!(*a, "-f" | "--force" | "--force-with-lease"))
        {
            self.findings.push(confirm(
                "`git push --force` overwrites remote commit history".to_string(),
            ));
            return;
        }
        let low_reason = if subcmd == "stash" && args.contains(&"drop") {
            Some("`git stash drop` discards stashed changes")
        } else if subcmd == "branch" && args.iter().any(|a| matches!(*a, "-D" | "-d" | "--delete"))
        {
            Some("`git branch -D` deletes a git branch")
        } else if subcmd == "clean" {
            Some("`git clean` discards untracked files inside repository")
        } else if subcmd == "reset" && args.contains(&"--hard") {
            Some("`git reset --hard` discards uncommitted changes inside repository")
        } else if subcmd == "checkout" && args.contains(&"--") && args.contains(&".") {
            Some("`git checkout -- .` discards uncommitted changes inside repository")
        } else {
            None
        };
        if let Some(reason) = low_reason {
            self.findings.push(RiskFinding {
                level: RiskLevel::Low,
                reason: reason.to_string(),
                target: None,
            });
        }
    }
}

/// The words of an `-exec`-style command, up to its `;` or `+`.
fn until_terminator(words: &[Token]) -> &[Token] {
    let end = words
        .iter()
        .position(|t| matches!(t.text.as_str(), ";" | "+"))
        .unwrap_or(words.len());
    &words[..end]
}

/// The program `words` finally runs, through wrappers that pass an argv on.
fn program(words: &[Token]) -> Option<String> {
    let mut words = words;
    loop {
        let unwrapped = wrappers::unwrap(words);
        match unwrapped.map(|u| u.inner) {
            Some(Inner::Argv(rest)) => words = rest,
            Some(_) => return words.first().map(Token::basename),
            None => return words.first().map(Token::basename),
        }
    }
}
