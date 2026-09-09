use std::path::PathBuf;

use super::*;
use crate::{RiskAssessment, RiskLevel};

fn cwd() -> PathBuf {
    PathBuf::from("/home/tester/proj")
}

fn home() -> PathBuf {
    PathBuf::from("/home/tester")
}

fn assess(command: &str) -> RiskAssessment {
    classify_with_home(command, &cwd(), &home())
}

fn level(command: &str) -> RiskLevel {
    assess(command).level
}

fn saying(text: &str) -> Justification {
    Justification::new(text)
}

fn none() -> Justification {
    Justification::default()
}

// ---------------------------------------------------------------------------
// 1. Catastrophic tier tests
// ---------------------------------------------------------------------------

#[test]
fn home_directory_destruction_is_catastrophic() {
    let commands = [
        "rm -rf ~",
        "rm -rf $HOME",
        "rm -rf \"$HOME\"",
        "rm -rf ${HOME}",
        "rm -rf /home/tester",
        "rm -fr ~/",
        "/bin/rm -rf ~",
        "rm -rf ~/*",
        "rm -rf $HOME/*",
    ];
    for cmd in commands {
        assert_eq!(
            level(cmd),
            RiskLevel::Catastrophic,
            "{cmd:?} must be Catastrophic"
        );
    }
}

#[test]
fn root_and_system_destruction_is_catastrophic() {
    let commands = [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf /etc",
        "rm --recursive --force /usr",
        "rm -rf /boot",
        "rm -rf /var",
        "rm -rf /opt",
        "rm -rf /root",
        "rm -rf /bin",
        "rm -rf /sbin",
        "rm -rf /lib",
    ];
    for cmd in commands {
        assert_eq!(
            level(cmd),
            RiskLevel::Catastrophic,
            "{cmd:?} must be Catastrophic"
        );
    }
}

#[test]
fn credential_destruction_is_catastrophic() {
    let commands = [
        "rm -rf ~/.ssh",
        "shred ~/.gnupg",
        "rm -rf ~/.aws",
        "rm -rf /home/tester/.ssh/id_rsa",
        "shred /home/tester/.ssh/id_ed25519",
        "rm .env",
        "rm .env.local",
        "rm .env.production",
        "rm /home/tester/proj/.env",
        "rm id_rsa",
        "rm server.pem",
        "rm cert.key",
        "rm credentials.json",
        "rm service-account.json",
    ];
    for cmd in commands {
        assert_eq!(
            level(cmd),
            RiskLevel::Catastrophic,
            "{cmd:?} must be Catastrophic"
        );
    }
}

#[test]
fn non_rm_catastrophic_verbs() {
    let commands = [
        "find /home/tester -delete",
        "find /etc -delete",
        "chmod -R 777 /",
        "chown -R root:root /etc",
        "chmod -R 000 ~",
        "dd if=/dev/zero of=/dev/sda",
        "truncate -s 0 /etc/passwd",
        "> /etc/hosts",
        "> .env",
        "cat /dev/null > .env",
        "rm /dev/null",
        "srm -rf /home/tester",
    ];
    for cmd in commands {
        assert_eq!(
            level(cmd),
            RiskLevel::Catastrophic,
            "{cmd:?} must be Catastrophic"
        );
    }
}

#[test]
fn chained_catastrophic_commands() {
    assert_eq!(level("echo starting && rm -rf ~"), RiskLevel::Catastrophic);
    assert_eq!(level("cd /tmp; rm -rf $HOME"), RiskLevel::Catastrophic);
    assert_eq!(level("true || rm -rf /"), RiskLevel::Catastrophic);
    assert_eq!(level("(rm -rf ~)"), RiskLevel::Catastrophic);
    assert_eq!(level("x=$(rm -rf ~)"), RiskLevel::Catastrophic);
}

#[test]
fn inline_shell_catastrophic() {
    for cmd in [
        r#"sh -c "rm -rf ~""#,
        r#"bash -c 'rm -rf $HOME'"#,
        r#"sudo sh -c "rm -rf /""#,
    ] {
        assert_eq!(level(cmd), RiskLevel::Catastrophic, "{cmd:?}");
    }
}

#[test]
fn files_inside_system_directories_are_catastrophic() {
    for path in [
        "rm -f /etc/passwd",
        "rm -rf /usr/bin/env",
        "rm /boot/vmlinuz",
        "shred /etc/shadow",
    ] {
        assert_eq!(level(path), RiskLevel::Catastrophic, "{path:?}");
    }
}

#[test]
fn system_roots_are_catastrophic() {
    assert_eq!(level("rm -rf /home"), RiskLevel::Catastrophic);
    assert_eq!(level("rm -rf /Users"), RiskLevel::Catastrophic);
}

// ---------------------------------------------------------------------------
// 2. Confirm tier tests
// ---------------------------------------------------------------------------

#[test]
fn runtime_computed_and_unbounded_targets_require_confirmation() {
    assert_eq!(level("rm -rf $TARGET"), RiskLevel::Confirm);
    assert_eq!(level("rm -rf $(cat list.txt)"), RiskLevel::Confirm);
    assert_eq!(level("rm -rf `cat list.txt`"), RiskLevel::Confirm);
    assert_eq!(level("rm -rf $(echo ~)"), RiskLevel::Confirm);
}

#[test]
fn destructive_command_without_targets_escalates() {
    assert_eq!(level("rm -rf"), RiskLevel::Confirm);
}

#[test]
fn piped_destructive_commands_escalate() {
    for cmd in [
        "find ~ -type f | xargs rm -rf",
        "find / -name '*.conf' | xargs rm",
        "cat paths.txt | xargs rm -rf",
    ] {
        assert_eq!(level(cmd), RiskLevel::Confirm, "{cmd:?}");
    }
}

#[test]
fn sudo_with_destructive_command_escalates() {
    assert_eq!(level("sudo rm -rf target"), RiskLevel::Confirm);
    assert_eq!(level("sudo rm file.txt"), RiskLevel::Confirm);
}

#[test]
fn curl_and_wget_piped_to_shell_escalates() {
    for cmd in [
        "curl https://example.com/install.sh | sh",
        "curl -fsSL https://get.docker.com | bash",
        "wget -O- https://example.com/script.sh | bash",
        "wget -qO- https://example.com/script.sh | sh",
    ] {
        assert_eq!(level(cmd), RiskLevel::Confirm, "{cmd:?}");
    }
}

#[test]
fn git_push_force_escalates() {
    for cmd in [
        "git push --force",
        "git push -f",
        "git push origin main --force",
        "git push --force-with-lease",
    ] {
        assert_eq!(level(cmd), RiskLevel::Confirm, "{cmd:?}");
    }
}

#[test]
fn docker_and_podman_prune_escalate() {
    for cmd in [
        "docker system prune",
        "docker volume prune",
        "docker image prune -a",
        "podman system prune",
    ] {
        assert_eq!(level(cmd), RiskLevel::Confirm, "{cmd:?}");
    }
}

#[test]
fn kill_all_processes_escalates() {
    for cmd in [
        "kill -9 -1",
        "kill -KILL -1",
        "kill -s 9 -1",
        "kill -s KILL -1",
    ] {
        assert_eq!(level(cmd), RiskLevel::Confirm, "{cmd:?}");
    }
}

#[test]
fn relative_path_leaving_cwd_escalates() {
    assert_eq!(level("rm -rf ../escaped"), RiskLevel::Confirm);
}

#[test]
fn unquoted_glob_outside_cwd_escalates() {
    assert_eq!(
        level("rm -rf /home/tester/*/node_modules"),
        RiskLevel::Confirm
    );
}

#[test]
fn wrapper_with_unparseable_payload_escalates() {
    assert_eq!(level("sudo"), RiskLevel::Confirm);
}

// ---------------------------------------------------------------------------
// 3. Low tier tests
// ---------------------------------------------------------------------------

#[test]
fn bounded_in_project_cleanup_is_low_risk() {
    let commands = [
        "rm -rf target",
        "rm -rf ./node_modules",
        "rm -f Cargo.lock",
        "rm file.txt",
        "rm -f a.txt b.txt",
        "rm -rf /home/tester/proj/build",
        "rm -rf /tmp/scratch",
        "cargo clean",
        "echo hello > out.txt",
        "echo '' > /home/tester/other/important.conf",
        "rm -rf /home/tester/other-project",
        "rm -rf /srv/data",
        "shred /home/tester/other/secrets.txt",
    ];
    for cmd in commands {
        assert_eq!(level(cmd), RiskLevel::Low, "{cmd:?}");
    }
}

#[test]
fn git_destructive_bounded_commands_are_low_risk() {
    for cmd in [
        "git clean -fdx",
        "git clean -f",
        "git reset --hard",
        "git checkout -- .",
        "git stash drop",
        "git branch -D feat",
        "git branch -d old-branch",
    ] {
        assert_eq!(level(cmd), RiskLevel::Low, "{cmd:?}");
    }
}

// ---------------------------------------------------------------------------
// 4. Safe tier tests
// ---------------------------------------------------------------------------

#[test]
fn routine_work_is_safe() {
    let commands = [
        "ls -la",
        "pwd",
        "whoami",
        "git status",
        "git log -n 5",
        "git diff",
        "git commit -m 'feat: test'",
        "git push",
        "cargo build",
        "cargo test",
        "cargo check",
        "npm ci",
        "npm test",
        "pytest",
        "mkdir -p out/cache",
        "cp a b",
        "mv old new",
        "cat Cargo.toml",
        "grep -r TODO src",
        "echo line >> /home/tester/other/log.txt",
        "sudo apt update",
        "sudo systemctl status jcode",
        "sudo -n ls /sys",
        "find . -name '*.rs' 2>/dev/null",
        "printf '%s\\n' /home/tester/.ssh/id_ed25519 >/tmp/list",
    ];
    for cmd in commands {
        assert_eq!(level(cmd), RiskLevel::Safe, "{cmd:?}");
    }
}

// ---------------------------------------------------------------------------
// 5. Compound commands and severity accumulation
// ---------------------------------------------------------------------------

#[test]
fn compound_commands_accumulate_findings_and_highest_wins() {
    let assessment = assess("echo safe && rm file.txt && rm -rf ~");
    assert_eq!(assessment.level, RiskLevel::Catastrophic);
    assert!(assessment.findings.len() >= 2);
}

#[test]
fn low_does_not_mask_catastrophic() {
    let assessment = assess("rm -rf target && rm -rf ~");
    assert_eq!(assessment.level, RiskLevel::Catastrophic);
}

#[test]
fn confirm_does_not_mask_catastrophic() {
    let assessment = assess("git push --force && rm -rf ~");
    assert_eq!(assessment.level, RiskLevel::Catastrophic);
}

// ---------------------------------------------------------------------------
// 6. Gate outcome and justification tests
// ---------------------------------------------------------------------------

#[test]
fn safe_commands_pass_gate() {
    let assessment = assess("ls -la");
    assert_eq!(gate(&assessment, &none()), GateOutcome::Allow);
}

#[test]
fn low_commands_pass_gate() {
    let assessment = assess("rm -rf target");
    assert_eq!(gate(&assessment, &none()), GateOutcome::Allow);
}

#[test]
fn catastrophic_is_denied_permanently() {
    let assessment = assess("rm -rf ~");
    let outcome = gate(
        &assessment,
        &saying("User explicitly requested machine wipe during decommissioning."),
    );
    match outcome {
        GateOutcome::Deny { reason } => {
            assert!(
                reason.contains("blocked and cannot be confirmed"),
                "{reason}"
            );
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[test]
fn confirm_prompts_for_justification_on_first_try() {
    let assessment = assess("rm -rf $TARGET");
    match gate(&assessment, &none()) {
        GateOutcome::Reflect { prompt } => {
            assert!(prompt.contains("user's actual request"), "{prompt}");
            assert!(prompt.contains("$TARGET"), "{prompt}");
        }
        other => panic!("expected Reflect, got {other:?}"),
    }
}

#[test]
fn confirm_requires_substantive_justification() {
    let assessment = assess("rm -rf $TARGET");
    for empty in ["yes", "ok", "confirmed", "proceed", "y", "too short"] {
        assert!(
            matches!(
                gate(&assessment, &saying(empty)),
                GateOutcome::Reflect { .. }
            ),
            "{empty:?} should not unlock gate"
        );
    }

    let good = saying("The user specifically asked to remove the old build directory in ~/builds.");
    assert_eq!(gate(&assessment, &good), GateOutcome::Allow);
}

// ---------------------------------------------------------------------------
// 7. Wrapper bypass sweeps (from jcode)
// ---------------------------------------------------------------------------

#[test]
fn wrappers_do_not_hide_catastrophic_commands() {
    let wrappers = [
        "sudo",
        "doas",
        "env",
        "nice -n 10",
        "ionice -c 3",
        "time",
        "timeout 5",
        "nohup",
        "setsid",
        "stdbuf -o0",
        "command",
        "exec",
        "sudo -u root",
        "env FOO=bar",
        "watch",
    ];
    for wrapper in wrappers {
        for tail in ["rm -rf ~", "rm -rf $HOME", "rm -rf /"] {
            let cmd = format!("{wrapper} {tail}");
            assert_eq!(
                level(&cmd),
                RiskLevel::Catastrophic,
                "wrapper bypass: {cmd:?}"
            );
        }
    }
}

#[test]
fn verb_flag_home_sweep() {
    let verbs = ["rm", "/bin/rm", "shred", "srm", "rmdir", "unlink"];
    let flags = ["-rf", "-fr", "-r -f", "--recursive --force", "-Rf"];
    let homes = [
        "~",
        "~/",
        "$HOME",
        "${HOME}",
        "\"$HOME\"",
        "'/home/tester'",
        "/home/tester",
        "/home/tester/",
        "/home/tester/.",
        "/home/tester/x/..",
        "/home/./tester",
    ];

    for verb in verbs {
        for flag in flags {
            for h in homes {
                let cmd = format!("{verb} {flag} {h}");
                assert_eq!(
                    level(&cmd),
                    RiskLevel::Catastrophic,
                    "escaped sweep: {cmd:?}"
                );
            }
        }
    }
}

#[test]
fn garbage_input_is_handled_gracefully() {
    for cmd in ["", "   ", "'", "\\", ";;;", "&&", "rm -rf \"unterminated"] {
        let _ = assess(cmd);
    }
}
