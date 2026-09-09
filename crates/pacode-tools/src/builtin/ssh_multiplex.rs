//! OpenSSH connection multiplexing check for shell commands.
//!
//! Without `ControlMaster`, each `ssh host cmd` pays for a full TCP handshake,
//! key exchange, and authentication. This module detects `ssh` commands targeting
//! hosts that lack connection multiplexing and raises a user-visible notice once
//! per daemon run.

use std::sync::atomic::{AtomicBool, Ordering};

use pacode_types::ToastLevel;

use crate::ToolHost;

/// Flags taking an option value in OpenSSH CLI (`ssh(1)`).
const VALUE_FLAGS: &[char] = &[
    'o', 'p', 'i', 'l', 'F', 'J', 'L', 'R', 'D', 'W', 'w', 'b', 'c', 'm', 'E', 'S', 'Q',
];

/// Global flag ensuring the connection multiplexing warning is shown at most once
/// per daemon lifetime.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Returns whether the multiplexing warning has already been emitted during this daemon run.
pub fn has_warned() -> bool {
    WARNED.load(Ordering::Relaxed)
}

/// Reset the warning flag for tests.
#[cfg(test)]
#[allow(dead_code)]
pub fn reset_warned_flag() {
    WARNED.store(false, Ordering::SeqCst);
}

/// The warning message suggested to the user when OpenSSH multiplexing is off.
pub fn warning_message(host: &str) -> String {
    format!(
        "OpenSSH connection multiplexing is off for '{host}'. \
         Every such command pays for a full handshake and authentication.\n\
         To fix this, add the following to ~/.ssh/config:\n\n\
         Host *\n    \
             ControlMaster auto\n    \
             ControlPath ~/.ssh/cm-%r@%h:%p\n    \
             ControlPersist 10m"
    )
}

/// Interpret `ssh -G` output.
///
/// Multiplexing is off when `controlmaster` is `no`/`false`/absent, or when
/// `controlpath` is `none`/absent.
pub fn is_multiplexing_enabled(output: &str) -> bool {
    let mut control_master: Option<&str> = None;
    let mut control_path: Option<&str> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        let Some(key) = parts.next() else { continue };
        if key.eq_ignore_ascii_case("controlmaster")
            && let Some(val) = parts.next()
        {
            control_master = Some(val);
        } else if key.eq_ignore_ascii_case("controlpath")
            && let Some(val) = parts.next()
        {
            control_path = Some(val);
        }
    }

    let cm_enabled = match control_master {
        Some(val) => !val.eq_ignore_ascii_case("no") && !val.eq_ignore_ascii_case("false"),
        None => false,
    };

    let cp_enabled = match control_path {
        Some(val) => !val.eq_ignore_ascii_case("none") && !val.is_empty(),
        None => false,
    };

    cm_enabled && cp_enabled
}

/// Check if a word represents a POSIX shell environment variable assignment (e.g. `FOO=bar`).
fn is_env_var_assignment(token: &str) -> bool {
    let Some((key, _)) = token.split_once('=') else {
        return false;
    };
    if key.is_empty() {
        return false;
    }
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate whether a candidate string can be confidently identified as a target host.
/// Returns false if ambiguous, containing shell substitutions, or invalid characters.
fn is_valid_host(host: &str) -> bool {
    if host.is_empty() || host.starts_with('-') {
        return false;
    }

    // Disallow shell expansions, wildcards, operators, quotes, whitespace
    if host.contains('$')
        || host.contains('`')
        || host.contains('{')
        || host.contains('}')
        || host.contains('(')
        || host.contains(')')
        || host.contains('\'')
        || host.contains('"')
        || host.contains('\\')
        || host.contains(';')
        || host.contains('&')
        || host.contains('|')
        || host.contains('<')
        || host.contains('>')
        || host.contains('*')
        || host.contains('?')
        || host.chars().any(|c| c.is_whitespace())
    {
        return false;
    }

    // Must contain only characters valid in hostnames, domain names, IPv4, or IPv6
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '[' | ']'))
    {
        return false;
    }

    // Must have at least one alphanumeric character
    host.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Extract host portion from a target argument (handling `ssh://[user@]host[:port]` and `[user@]host`).
fn parse_host_candidate(token: &str) -> Option<String> {
    let mut candidate = token.trim();

    if let Some(rest) = candidate.strip_prefix("ssh://") {
        candidate = rest;
        if let Some((_, h)) = candidate.rsplit_once('@') {
            candidate = h;
        }
        if let Some((h, _)) = candidate.split_once(':') {
            candidate = h;
        }
    } else if let Some((_, h)) = candidate.rsplit_once('@') {
        candidate = h;
    }

    let host = candidate.trim();
    if is_valid_host(host) {
        Some(host.to_string())
    } else {
        None
    }
}

/// Parse the arguments of an `ssh` invocation to extract the destination host.
///
/// Returns `None` if the command does not specify a confident destination host,
/// if it is an `ssh -G` query command, or if flags are malformed.
fn extract_host_from_ssh_args<'a>(
    tokens: impl Iterator<Item = &'a pacode_risk::tokenize::Token>,
) -> Option<String> {
    let mut iter = tokens.peekable();
    let mut options_ended = false;

    while let Some(token) = iter.next() {
        if token.is_operator {
            continue;
        }

        let text = token.text.trim();
        if text.is_empty() {
            continue;
        }

        if !options_ended {
            if text == "--" {
                options_ended = true;
                continue;
            }

            if text == "-G" || text.starts_with("-G") {
                // Query configuration only; does not connect.
                return None;
            }

            if let Some(flag_body) = text.strip_prefix('-') {
                if flag_body.is_empty() {
                    return None;
                }

                if flag_body.starts_with('-') {
                    // Unknown long option; not a destination host
                    continue;
                }

                // Check for value-taking flags
                let chars: Vec<char> = flag_body.chars().collect();
                if let Some(pos) = chars.iter().position(|c| VALUE_FLAGS.contains(c))
                    && pos == chars.len() - 1
                {
                    // Flag is at end of token (e.g. `-p`, `-o`, `-vp`), consumes next token
                    let next_token = iter.next()?;
                    if next_token.is_operator {
                        return None;
                    }
                    // If pos < chars.len() - 1, value was attached (e.g. `-p2222`, `-oBatchMode=yes`)
                }
                continue;
            }
        }

        // First non-flag token is the target destination
        return parse_host_candidate(text);
    }

    None
}

/// Extract target host(s) from a shell command string that invokes ssh.
///
/// Supports pipelines, conditional operators (`&&`, `||`), sequential chains (`;`),
/// environment variable assignments, and flags that take values.
pub fn extract_ssh_hosts(command: &str) -> Vec<String> {
    let segments = pacode_risk::tokenize::split_segments(command);
    let mut hosts = Vec::new();

    for segment in segments {
        let mut tokens = segment.iter().filter(|t| !t.text.trim().is_empty());

        // Skip leading environment variable assignments (e.g. `VAR=1 ssh host`)
        let Some(first_cmd) = tokens.find(|t| !is_env_var_assignment(&t.text)) else {
            continue;
        };

        let cmd_name = first_cmd.basename();
        if cmd_name != "ssh" {
            continue;
        }

        if let Some(host) = extract_host_from_ssh_args(tokens) {
            hosts.push(host);
        }
    }

    hosts
}

/// Extract the first confident target host from a shell command that invokes ssh.
pub fn extract_ssh_host(command: &str) -> Option<String> {
    extract_ssh_hosts(command).into_iter().next()
}

/// Pure helper for verifying check-and-warn logic and the once-only flag in tests.
#[cfg(test)]
pub fn check_and_warn_pure<F, N>(
    command: &str,
    warned: &AtomicBool,
    mut check_host_multiplexing: F,
    mut emit_notice: N,
) where
    F: FnMut(&str) -> Option<bool>,
    N: FnMut(&str, &str),
{
    if warned.load(Ordering::Relaxed) {
        return;
    }

    let Some(target_host) = extract_ssh_host(command) else {
        return;
    };

    if warned.load(Ordering::Relaxed) {
        return;
    }

    let Some(enabled) = check_host_multiplexing(&target_host) else {
        return;
    };

    if !enabled
        && warned
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        emit_notice(&target_host, &warning_message(&target_host));
    }
}

/// Check if the shell command invokes SSH on a host without connection multiplexing.
/// If multiplexing is off, emit a warning notice to the user once per daemon lifetime.
pub async fn check_and_warn(command: &str, host: &dyn ToolHost) {
    // Fast check: if warning was already emitted during this daemon lifetime, skip everything
    if has_warned() {
        return;
    }

    // Check if command invokes ssh and extract target host
    let Some(target_host) = extract_ssh_host(command) else {
        return;
    };

    // Double check flag before spawning process
    if has_warned() {
        return;
    }

    let host_clone = target_host.clone();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("ssh")
            .arg("-G")
            .arg(&host_clone)
            .output()
    })
    .await;

    let Ok(Ok(output)) = output else {
        // Process spawn failed or ssh not found; produce no warning
        return;
    };

    if !output.status.success() {
        // Non-zero exit: produce no warning
        return;
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let enabled = is_multiplexing_enabled(&stdout_str);

    if !enabled
        && WARNED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        host.emit_notice(ToastLevel::Warn, warning_message(&target_host));
    }
}
