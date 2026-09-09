//! Command risk classification entry point.
//!
//! Evaluates commands by blast radius, unwraps execution wrappers, and resolves
//! targets against cwd and home.

use std::path::{Path, PathBuf};

use crate::paths::{self, is_safe_redirect_sink};
use crate::tokenize::{self, Token};
use crate::{RiskAssessment, RiskFinding, RiskLevel};

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;

/// Commands that destroy data as their primary purpose.
const DESTRUCTIVE_COMMANDS: &[&str] = &[
    "rm", "rmdir", "shred", "unlink", "truncate", "dd", "mkfs", "fdisk", "parted", "wipefs", "srm",
];

/// Commands that execute another program supplied in arguments.
const WRAPPER_COMMANDS: &[&str] = &[
    "sudo", "doas", "env", "nice", "ionice", "time", "timeout", "nohup", "xargs", "command",
    "builtin", "exec", "setsid", "stdbuf", "chroot", "su", "watch", "eval",
];

/// Shell grammar keywords that may precede the command name.
const SHELL_CONTROL_PREFIXES: &[&str] = &[
    "then", "do", "else", "elif", "if", "while", "until", "case", "in", "select",
];

/// Shell binaries that accept inline scripts.
const SHELL_COMMANDS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// Commands destructive only with specific flags.
const CONDITIONALLY_DESTRUCTIVE: &[(&str, &[&str])] = &[
    ("find", &["-delete", "-exec", "-execdir", "-ok", "-okdir"]),
    ("git", &["clean"]),
    ("chmod", &["-R", "--recursive"]),
    ("chown", &["-R", "--recursive"]),
];

/// Outcome of the reflection gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Allow,
    Reflect { prompt: String },
    Deny { reason: String },
}

/// A justification supplied by the model on a reflection turn.
#[derive(Debug, Clone, Default)]
pub struct Justification {
    pub text: Option<String>,
}

impl Justification {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
        }
    }

    pub fn is_substantive(&self) -> bool {
        let Some(text) = self.text.as_deref().map(str::trim) else {
            return false;
        };
        if text.len() < MIN_JUSTIFICATION_LEN {
            return false;
        }
        const EMPTY_AFFIRMATIONS: &[&str] = &[
            "yes",
            "ok",
            "okay",
            "sure",
            "confirmed",
            "proceed",
            "do it",
            "continue",
            "y",
            "approved",
            "go ahead",
        ];
        let lowered = text.to_lowercase();
        let stripped = lowered.trim_end_matches(['.', '!', '?']).trim();
        !EMPTY_AFFIRMATIONS.contains(&stripped)
    }
}

const MIN_JUSTIFICATION_LEN: usize = 25;

/// Decide whether an assessed command runs, reflects, or is permanently denied.
pub fn gate(assessment: &RiskAssessment, justification: &Justification) -> GateOutcome {
    match assessment.level {
        RiskLevel::Safe | RiskLevel::Low => GateOutcome::Allow,
        RiskLevel::Catastrophic => {
            let expl = assessment.explanation();
            GateOutcome::Deny {
                reason: format!(
                    "This command is blocked and cannot be confirmed.\n\n{expl}\n\
                     If the user genuinely wants this, they must run it themselves outside the agent."
                ),
            }
        }
        RiskLevel::Confirm => {
            if justification.is_substantive() {
                GateOutcome::Allow
            } else {
                GateOutcome::Reflect {
                    prompt: reflection_prompt(assessment),
                }
            }
        }
    }
}

fn reflection_prompt(assessment: &RiskAssessment) -> String {
    let expl = assessment.explanation();
    format!(
        "This command was not run. It is irreversible:\n\n{expl}\n\
         Before it can proceed, stop and check it against the user's actual request:\n\
         - Which specific thing the user asked for requires deleting this?\n\
         - Did the user name this path, or did you infer it?\n\
         - If you inferred it, is a narrower target enough?\n\
         - If this is wrong, nothing here can be recovered.\n\n\
         If it is genuinely what the user asked for, re-issue the same call with a \
         `justification` field explaining which request it serves. If you are not sure, \
         ask the user instead: that costs one message, and being wrong costs their data."
    )
}

/// Classify command using `HOME` from the environment.
pub fn classify(command: &str, cwd: &Path) -> RiskAssessment {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    classify_with_home(command, cwd, &home)
}

/// Classify command with an explicit cwd and home directory.
pub fn classify_with_home(command: &str, cwd: &Path, home: &Path) -> RiskAssessment {
    let mut findings = Vec::new();
    let segments = tokenize::split_segments(command);

    // Detect curl ... | sh or wget ... | bash pipeline patterns
    for window in segments.windows(2) {
        let seg1 = &window[0];
        let seg2 = &window[1];
        if let (Some(first1), Some(first2)) = (seg1.first(), seg2.first()) {
            let cmd1 = first1.basename();
            let cmd2 = first2.basename();
            if matches!(cmd1.as_str(), "curl" | "wget" | "fetch")
                && first2.receives_pipe
                && SHELL_COMMANDS.contains(&cmd2.as_str())
            {
                findings.push(RiskFinding {
                    level: RiskLevel::Confirm,
                    reason: format!(
                        "`{cmd1} | {cmd2}` downloads and executes a remote script directly \
                         in the shell"
                    ),
                    target: None,
                });
            }
        }
    }

    for segment in &segments {
        assess_segment(segment, cwd, home, &mut findings, false);
    }

    if findings.is_empty() {
        RiskAssessment::safe()
    } else {
        RiskAssessment::from_findings(findings)
    }
}

fn wrapper_flag_takes_value(wrapper: &str, flag: &str) -> bool {
    match wrapper {
        "sudo" | "doas" => matches!(flag, "-u" | "--user" | "-g" | "--group" | "-C"),
        "nice" => matches!(flag, "-n" | "--adjustment"),
        "ionice" => matches!(
            flag,
            "-c" | "--class" | "-n" | "--classdata" | "-p" | "--pid"
        ),
        "timeout" => matches!(flag, "-s" | "--signal" | "-k" | "--kill-after"),
        "xargs" => matches!(
            flag,
            "-n" | "--max-args"
                | "-P"
                | "--max-procs"
                | "-s"
                | "--max-chars"
                | "-d"
                | "-E"
                | "-I"
                | "-L"
        ),
        "chroot" => matches!(flag, "--userspec" | "--groups"),
        _ => false,
    }
}

fn assess_segment(
    tokens: &[Token],
    cwd: &Path,
    home: &Path,
    findings: &mut Vec<RiskFinding>,
    inherited_sudo: bool,
) {
    let mut tokens = tokens;
    while tokens.len() > 1
        && tokens
            .first()
            .is_some_and(|token| SHELL_CONTROL_PREFIXES.contains(&token.text.as_str()))
    {
        tokens = &tokens[1..];
    }

    let mut was_sudo = inherited_sudo;
    let mut was_xargs = false;
    let mut wrapped_by: Option<String> = None;

    loop {
        let Some(first) = tokens.first() else {
            if let Some(wrapper) = wrapped_by {
                findings.push(RiskFinding {
                    level: RiskLevel::Confirm,
                    reason: format!(
                        "`{wrapper}` runs another command that could not be \
                         identified statically"
                    ),
                    target: None,
                });
            }
            return;
        };
        let name = first.basename();
        if !WRAPPER_COMMANDS.contains(&name.as_str()) {
            break;
        }
        if name == "sudo" || name == "doas" {
            was_sudo = true;
        }
        if name == "xargs" {
            was_xargs = true;
        }
        wrapped_by = Some(name.clone());

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
                if wrapper_flag_takes_value(&name, &token.text) && idx < rest.len() {
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
        tokens = &rest[idx..];
    }

    let Some(program) = tokens.first() else {
        if let Some(wrapper) = wrapped_by {
            findings.push(RiskFinding {
                level: RiskLevel::Confirm,
                reason: format!(
                    "`{wrapper}` runs another command that could not be \
                     identified statically"
                ),
                target: None,
            });
        }
        return;
    };
    let program_name = program.basename();

    // Inline shell scripts
    if SHELL_COMMANDS.contains(&program_name.as_str()) {
        for token in tokens.iter().skip(1).filter(|t| !t.is_flag()) {
            for segment in tokenize::split_segments(&token.text) {
                assess_segment(&segment, cwd, home, findings, was_sudo);
            }
        }
        return;
    }

    // Special-case tool classification (git, cargo, docker, kill)
    classify_known_tools(program_name.as_str(), tokens, findings);

    let is_destructive =
        DESTRUCTIVE_COMMANDS.contains(&program_name.as_str()) || program_name.starts_with("mkfs.");

    let conditional_flags = CONDITIONALLY_DESTRUCTIVE
        .iter()
        .find(|(name, _)| *name == program_name)
        .map(|(_, flags)| *flags);

    let triggered = if is_destructive {
        true
    } else if let Some(flags) = conditional_flags {
        tokens.iter().any(|t| flags.contains(&t.text.as_str()))
    } else {
        false
    };

    if was_xargs && triggered {
        findings.push(RiskFinding {
            level: RiskLevel::Confirm,
            reason: format!(
                "`xargs {program_name}` executes destructive operations from standard input"
            ),
            target: None,
        });
    }

    if triggered && was_sudo {
        findings.push(RiskFinding {
            level: RiskLevel::Confirm,
            reason: format!(
                "`sudo` runs destructive command `{program_name}` with elevated privileges"
            ),
            target: None,
        });
    }

    let redirect_targets: Vec<&Token> = tokens
        .iter()
        .filter(|t| t.is_truncating_redirect_target)
        .collect();

    if !triggered && redirect_targets.is_empty() {
        return;
    }

    let mut targets: Vec<(&Token, bool)> = if triggered {
        tokens
            .iter()
            .skip(1)
            .filter(|t| !t.is_flag() && !t.is_operator)
            .map(|t| (t, false))
            .collect()
    } else {
        Vec::new()
    };
    targets.extend(
        redirect_targets
            .iter()
            .copied()
            .filter(|target| !is_safe_redirect_sink(&target.text))
            .map(|t| (t, true)),
    );

    if triggered && tokens.first().is_some_and(|t| t.receives_pipe) {
        findings.push(RiskFinding {
            level: RiskLevel::Confirm,
            reason: format!(
                "`{program_name}` deletes paths supplied by a pipe, so the set of \
                 affected files cannot be checked before it runs"
            ),
            target: None,
        });
    }

    if triggered && targets.is_empty() {
        findings.push(RiskFinding {
            level: RiskLevel::Confirm,
            reason: format!(
                "`{program_name}` is destructive but its target could not be \
                 determined statically, so its blast radius is unknown"
            ),
            target: None,
        });
        return;
    }

    let recursive = triggered && tokens.iter().any(|t| t.is_recursive_flag());

    for (target, is_redirect) in targets {
        let raw = target
            .text
            .split_once('=')
            .filter(|(key, _)| matches!(*key, "of" | "if" | "seek" | "conv"))
            .map(|(_, value)| value)
            .unwrap_or(&target.text);

        if let Some(finding) = paths::classify_target(raw, recursive, cwd, home, is_redirect) {
            findings.push(finding);
        }
    }
}

fn classify_known_tools(program: &str, tokens: &[Token], findings: &mut Vec<RiskFinding>) {
    let args: Vec<&str> = tokens.iter().skip(1).map(|t| t.text.as_str()).collect();
    match program {
        "git" => {
            let subcmd = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .copied()
                .unwrap_or("");
            if subcmd == "push"
                && args
                    .iter()
                    .any(|a| matches!(*a, "-f" | "--force" | "--force-with-lease"))
            {
                findings.push(RiskFinding {
                    level: RiskLevel::Confirm,
                    reason: "`git push --force` overwrites remote commit history".to_string(),
                    target: None,
                });
            } else {
                let low_reason = if subcmd == "stash" && args.contains(&"drop") {
                    Some("`git stash drop` discards stashed changes")
                } else if subcmd == "branch"
                    && args.iter().any(|a| matches!(*a, "-D" | "-d" | "--delete"))
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
                    findings.push(RiskFinding {
                        level: RiskLevel::Low,
                        reason: reason.to_string(),
                        target: None,
                    });
                }
            }
        }
        "cargo" => {
            if args.contains(&"clean") {
                findings.push(RiskFinding {
                    level: RiskLevel::Low,
                    reason: "`cargo clean` deletes all build artifacts".to_string(),
                    target: None,
                });
            }
        }
        "docker" | "podman" => {
            if args.contains(&"prune") {
                findings.push(RiskFinding {
                    level: RiskLevel::Confirm,
                    reason: format!(
                        "`{program} prune` removes unused containers, images, or volumes"
                    ),
                    target: None,
                });
            }
        }
        "kill" => {
            let kills_all = args.contains(&"-1");
            let is_force = args
                .iter()
                .any(|a| matches!(*a, "-9" | "-KILL" | "-s" | "KILL" | "9"));
            if kills_all && is_force {
                findings.push(RiskFinding {
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
