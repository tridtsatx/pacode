//! Protected paths and catastrophic target detection.
//!
//! Classifies paths by blast radius: absolute protection for root, home,
//! system directories, and credential files.

use std::path::{Component, Path, PathBuf};

use crate::{RiskFinding, RiskLevel};

/// Credential stores protected recursively under home directory.
const PROTECTED_CREDENTIAL_SUBPATHS: &[&str] = &[".ssh", ".gnupg", ".aws", ".kube", ".docker"];

/// Directory roots under home protected wholesale, but individual files within them
/// are allowed to be modified.
const PROTECTED_HOME_SUBPATHS: &[&str] = &[
    ".config",
    ".local",
    ".local/share",
    ".jcode",
    ".claude",
    ".pacode",
    "Documents",
    "Desktop",
];

/// Absolute system roots that must never be destroyed.
const PROTECTED_SYSTEM_PATHS: &[&str] = &[
    "/",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib",
    "/lib64",
    "/opt",
    "/proc",
    "/root",
    "/sbin",
    "/srv",
    "/sys",
    "/usr",
    "/var",
    "/Applications",
    "/System",
    "/Library",
    "/Users",
    "/home",
];

/// System paths protected recursively: deleting any single file inside them is unacceptable.
const SYSTEM_PATHS_PROTECTED_RECURSIVELY: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/root", "/sbin", "/sys", "/usr",
    "/var/lib", "/System", "/Library",
];

/// The set of paths and patterns this policy protects.
pub struct ProtectedPaths;

impl ProtectedPaths {
    pub fn home_subpaths() -> &'static [&'static str] {
        PROTECTED_HOME_SUBPATHS
    }

    pub fn credential_subpaths() -> &'static [&'static str] {
        PROTECTED_CREDENTIAL_SUBPATHS
    }

    pub fn system_paths() -> &'static [&'static str] {
        PROTECTED_SYSTEM_PATHS
    }

    pub fn recursive_system_paths() -> &'static [&'static str] {
        SYSTEM_PATHS_PROTECTED_RECURSIVELY
    }

    /// True if the file name matches common credential, key, or secret naming patterns.
    pub fn is_credential_file_name(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            ".env"
                | "credentials.json"
                | "client_secret.json"
                | "service-account.json"
                | "service_account.json"
                | "id_rsa"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
        ) {
            return true;
        }
        if lower.starts_with(".env.") || lower.ends_with(".env") {
            return true;
        }
        if lower.starts_with("id_rsa")
            || lower.starts_with("id_ed25519")
            || lower.starts_with("id_ecdsa")
            || lower.starts_with("id_dsa")
        {
            return true;
        }
        if lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower.ends_with(".p12")
            || lower.ends_with(".pfx")
        {
            return true;
        }
        if lower.ends_with(".json")
            && (lower.contains("credentials")
                || lower.contains("client_secret")
                || lower.contains("service_account")
                || lower.contains("service-account"))
        {
            return true;
        }
        false
    }

    /// True if the given path represents a credential file.
    pub fn is_credential_file(path: &Path) -> bool {
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str())
            && Self::is_credential_file_name(file_name)
        {
            return true;
        }
        false
    }
}

/// Lexically resolve `.` and `..` without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        return PathBuf::from("/");
    }
    out
}

/// Resolve `~`, `$HOME`, and relative paths into an absolute, normalized path.
pub fn expand(raw: &str, cwd: &Path, home: &Path) -> PathBuf {
    let mut text = raw.to_string();

    if !home.as_os_str().is_empty() {
        let home_str = home.to_string_lossy().to_string();
        for var in ["${HOME}", "$HOME"] {
            text = text.replace(var, &home_str);
        }
        if text == "~" {
            text = home_str.clone();
        } else if let Some(rest) = text.strip_prefix("~/") {
            text = format!("{home_str}/{rest}");
        }
    }

    let path = PathBuf::from(&text);
    if path.is_absolute() {
        return normalize(&path);
    }
    if !cwd.as_os_str().is_empty() {
        normalize(&cwd.join(path))
    } else {
        path
    }
}

/// Whether destroying this path is categorically catastrophic.
pub fn is_catastrophic_target(path: &Path, home: &Path) -> bool {
    let path = normalize(path);

    if PROTECTED_SYSTEM_PATHS.iter().any(|p| path == Path::new(p)) {
        return true;
    }
    if SYSTEM_PATHS_PROTECTED_RECURSIVELY
        .iter()
        .any(|p| path.starts_with(p))
    {
        return true;
    }

    if ProtectedPaths::is_credential_file(&path) {
        return true;
    }

    if !home.as_os_str().is_empty() {
        let home = normalize(home);
        if path == home {
            return true;
        }
        if PROTECTED_CREDENTIAL_SUBPATHS
            .iter()
            .any(|sub| path.starts_with(home.join(sub)))
        {
            return true;
        }
        if PROTECTED_HOME_SUBPATHS
            .iter()
            .any(|sub| path == home.join(sub))
        {
            return true;
        }
    }

    false
}

/// Classify one operand or redirection destination.
pub fn classify_target(
    raw: &str,
    recursive: bool,
    cwd: &Path,
    home: &Path,
    is_redirect: bool,
) -> Option<RiskFinding> {
    let expanded = expand(raw, cwd, home);

    // 1. Wildcard / glob check
    if raw.contains('*') || raw.contains('?') {
        if let Some(parent) = expanded.parent()
            && is_catastrophic_target(parent, home)
            && expanded.file_name().is_some_and(|n| n == "*")
        {
            return Some(RiskFinding {
                level: RiskLevel::Catastrophic,
                reason: "would destroy the entire contents of a protected directory".to_string(),
                target: Some(raw.to_string()),
            });
        }
        if let Some(parent) = expanded.parent()
            && !parent.to_string_lossy().contains(['*', '?'])
        {
            let inside_cwd = parent.starts_with(normalize(cwd));
            if inside_cwd || is_temp_path(parent) {
                return Some(RiskFinding {
                    level: RiskLevel::Low,
                    reason: "glob is bounded to the working or temporary directory".to_string(),
                    target: Some(raw.to_string()),
                });
            }
        }
        return Some(RiskFinding {
            level: RiskLevel::Confirm,
            reason: "target contains an unquoted glob outside cwd, so the exact set of \
                     affected files is not known before execution"
                .to_string(),
            target: Some(raw.to_string()),
        });
    }

    // 2. Absolute catastrophic path check
    if is_catastrophic_target(&expanded, home) {
        return Some(RiskFinding {
            level: RiskLevel::Catastrophic,
            reason: "targets a protected system, home, or credentials path that must never \
                     be destroyed"
                .to_string(),
            target: Some(expanded.display().to_string()),
        });
    }

    // 3. Runtime computed variables or substitutions
    if raw.contains('$') || raw.contains('`') {
        return Some(RiskFinding {
            level: RiskLevel::Confirm,
            reason: "target is computed at runtime (variable or command substitution), so \
                     its value cannot be checked in advance"
                .to_string(),
            target: Some(raw.to_string()),
        });
    }

    // 4. Raw device node writes
    if expanded.starts_with("/dev") && !is_safe_redirect_sink(raw) {
        return Some(RiskFinding {
            level: RiskLevel::Catastrophic,
            reason: "writes directly to a device node, which can destroy a filesystem or disk"
                .to_string(),
            target: Some(expanded.display().to_string()),
        });
    }

    let norm_cwd = normalize(cwd);
    let inside_cwd = expanded.starts_with(&norm_cwd);

    // 5. Relative path containing '..' leaving cwd
    if raw.contains("..") && !inside_cwd {
        return Some(RiskFinding {
            level: RiskLevel::Confirm,
            reason: "relative path containing '..' leaves the working directory".to_string(),
            target: Some(raw.to_string()),
        });
    }

    // 6. Target inside cwd
    if inside_cwd {
        return Some(RiskFinding {
            level: RiskLevel::Low,
            reason: if recursive {
                "recursive delete inside the working directory".to_string()
            } else {
                "delete inside the working directory".to_string()
            },
            target: Some(expanded.display().to_string()),
        });
    }

    // 7. Temporary path
    if is_temp_path(&expanded) {
        if is_redirect {
            return None;
        }
        return Some(RiskFinding {
            level: RiskLevel::Low,
            reason: "destructive operation in temporary directory".to_string(),
            target: Some(expanded.display().to_string()),
        });
    }

    // 8. Concrete path outside cwd
    Some(RiskFinding {
        level: RiskLevel::Low,
        reason: "destructive operation targets a concrete path outside the working directory"
            .to_string(),
        target: Some(expanded.display().to_string()),
    })
}

/// True if `path` is inside standard temporary directories or `$TMPDIR`.
pub fn is_temp_path(path: &Path) -> bool {
    let prefixes = ["/tmp", "/var/tmp", "/private/tmp"];
    if prefixes.iter().any(|prefix| path.starts_with(prefix)) {
        return true;
    }
    if let Ok(tmpdir) = std::env::var("TMPDIR")
        && !tmpdir.is_empty()
        && path.starts_with(&tmpdir)
    {
        return true;
    }
    false
}

/// Bit buckets are safe redirect sinks.
pub fn is_safe_redirect_sink(raw: &str) -> bool {
    matches!(raw, "/dev/null" | "/dev/stdout" | "/dev/stderr" | "NUL")
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod paths_tests;
