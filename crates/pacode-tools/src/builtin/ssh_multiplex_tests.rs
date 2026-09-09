use std::sync::atomic::{AtomicBool, Ordering};

use super::ssh_multiplex::*;

#[test]
fn test_extract_ssh_host_standard_forms() {
    assert_eq!(extract_ssh_host("ssh host cmd"), Some("host".to_string()));
    assert_eq!(extract_ssh_host("ssh user@host"), Some("host".to_string()));
    assert_eq!(
        extract_ssh_host("ssh -p 2222 host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -o BatchMode=yes host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -i key -l user host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh user@host cmd arg1 arg2"),
        Some("host".to_string())
    );
}

#[test]
fn test_extract_ssh_host_in_pipelines_and_chains() {
    assert_eq!(
        extract_ssh_host("echo foo | ssh host bar"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("git pull && ssh host \"echo hi\""),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("make build && ssh user@remote.server.org ./deploy.sh"),
        Some("remote.server.org".to_string())
    );
    assert_eq!(
        extract_ssh_host("true || ssh fallback-box uptime"),
        Some("fallback-box".to_string())
    );
    assert_eq!(
        extract_ssh_host("cd /opt; ssh -p 2200 host"),
        Some("host".to_string())
    );
}

#[test]
fn test_extract_ssh_host_various_flags() {
    assert_eq!(
        extract_ssh_host("ssh -F /path/to/custom_config host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -J jumphost target"),
        Some("target".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -oBatchMode=yes host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -p2222 host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -v -p 2222 host"),
        Some("host".to_string())
    );
    assert_eq!(extract_ssh_host("ssh -Tv host"), Some("host".to_string()));
    assert_eq!(extract_ssh_host("ssh -4 -C host"), Some("host".to_string()));
    assert_eq!(
        extract_ssh_host("ssh -o ConnectTimeout=10 -o BatchMode=yes user@10.0.0.1"),
        Some("10.0.0.1".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -- host cmd"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -- user@host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ENV=1 OTHER=2 ssh host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -L 8080:localhost:80 -R 9090:localhost:90 host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -b 192.168.1.5 -c aes128-ctr -m hmac-sha2-256 host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("ssh -E /tmp/ssh.log -S /tmp/ctl.sock host"),
        Some("host".to_string())
    );
    assert_eq!(
        extract_ssh_host("/usr/bin/ssh host cmd"),
        Some("host".to_string())
    );
}

#[test]
fn test_forms_where_host_cannot_be_determined() {
    assert_eq!(extract_ssh_host("ssh"), None);
    assert_eq!(extract_ssh_host("ssh -p"), None);
    assert_eq!(extract_ssh_host("ssh -o"), None);
    assert_eq!(extract_ssh_host("ssh -v"), None);
    assert_eq!(extract_ssh_host("ssh -v -p 22"), None);
    assert_eq!(extract_ssh_host("ssh $HOST"), None);
    assert_eq!(extract_ssh_host("ssh \"$TARGET\""), None);
    assert_eq!(extract_ssh_host("ssh user@"), None);
    assert_eq!(extract_ssh_host("ssh -G host"), None);
    assert_eq!(extract_ssh_host("ssh --"), None);
    assert_eq!(extract_ssh_host("ssh -- -notahost"), None);
    assert_eq!(extract_ssh_host("ssh -"), None);
    assert_eq!(extract_ssh_host("ssh \"host;rm\""), None);
    assert_eq!(extract_ssh_host("ssh \"user@host$(whoami)\""), None);
    assert_eq!(extract_ssh_host("ssh user@*"), None);
}

#[test]
fn test_commands_merely_containing_word_ssh() {
    assert_eq!(extract_ssh_host("grep ssh notes.txt"), None);
    assert_eq!(extract_ssh_host("rsync -e ssh a b"), None);
    assert_eq!(
        extract_ssh_host("rsync -avz -e \"ssh -p 2222\" src/ user@dest:/path"),
        None
    );
    assert_eq!(extract_ssh_host("echo \"ssh host\""), None);
    assert_eq!(extract_ssh_host("cat ssh.log"), None);
    assert_eq!(extract_ssh_host("ssh-keygen -t ed25519"), None);
    assert_eq!(extract_ssh_host("ssh-copy-id host"), None);
    assert_eq!(extract_ssh_host("ssh-add ~/.ssh/id_rsa"), None);
}

#[test]
fn test_parse_ssh_g_output_multiplexing_on() {
    let output = "\
host example.com
user alice
controlmaster auto
controlpath ~/.ssh/cm-%r@%h:%p
controlpersist 10m
";
    assert!(is_multiplexing_enabled(output));

    let mixed_case = "\
ControlMaster Auto
ControlPath /tmp/ssh.sock
";
    assert!(is_multiplexing_enabled(mixed_case));
}

#[test]
fn test_parse_ssh_g_output_multiplexing_off() {
    let output_false = "\
host example.com
controlmaster false
controlpath ~/.ssh/cm-%r@%h:%p
";
    assert!(!is_multiplexing_enabled(output_false));

    let output_no = "\
host example.com
controlmaster no
controlpath ~/.ssh/cm-%r@%h:%p
";
    assert!(!is_multiplexing_enabled(output_no));

    let real_output = "\
host some-host
user someone
hostname some-host
port 22
controlmaster false
controlpersist no
";
    assert!(!is_multiplexing_enabled(real_output));
}

#[test]
fn test_parse_ssh_g_output_controlpath_none() {
    let output = "\
controlmaster auto
controlpath none
";
    assert!(!is_multiplexing_enabled(output));
}

#[test]
fn test_parse_ssh_g_output_missing_keys() {
    let no_controlmaster = "\
controlpath ~/.ssh/cm-%r@%h:%p
";
    assert!(!is_multiplexing_enabled(no_controlmaster));

    let no_controlpath = "\
controlmaster auto
";
    assert!(!is_multiplexing_enabled(no_controlpath));

    assert!(!is_multiplexing_enabled(""));
}

#[test]
fn test_once_only_flag_pure() {
    let warned = AtomicBool::new(false);
    let mut check_count = 0;
    let mut emitted = Vec::new();

    // First ssh command: multiplexing off -> warns
    check_and_warn_pure(
        "ssh host1 cmd",
        &warned,
        |_host| {
            check_count += 1;
            Some(false)
        },
        |host, msg| {
            emitted.push((host.to_string(), msg.to_string()));
        },
    );

    assert_eq!(check_count, 1);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].0, "host1");
    assert!(warned.load(Ordering::Relaxed));

    // Second ssh command: warned flag is already true -> no second check, no second warning
    check_and_warn_pure(
        "ssh host2 cmd",
        &warned,
        |_host| {
            check_count += 1;
            Some(false)
        },
        |host, msg| {
            emitted.push((host.to_string(), msg.to_string()));
        },
    );

    assert_eq!(check_count, 1, "check must NOT be spawned a second time");
    assert_eq!(emitted.len(), 1, "no second warning must be emitted");
}

#[test]
fn test_once_only_flag_multiplexing_enabled_does_not_warn() {
    let warned = AtomicBool::new(false);
    let mut check_count = 0;
    let mut emitted = Vec::new();

    check_and_warn_pure(
        "ssh host1 cmd",
        &warned,
        |_host| {
            check_count += 1;
            Some(true) // multiplexing enabled!
        },
        |host, msg| {
            emitted.push((host.to_string(), msg.to_string()));
        },
    );

    assert_eq!(check_count, 1);
    assert!(emitted.is_empty());
    assert!(!warned.load(Ordering::Relaxed));
}

#[test]
fn test_reset_warned_flag() {
    reset_warned_flag();
    assert!(!has_warned());
}

#[test]
fn test_warning_message_format() {
    let msg = warning_message("my-server.example.com");
    assert!(msg.contains("OpenSSH connection multiplexing is off for 'my-server.example.com'"));
    assert!(msg.contains("Every such command pays for a full handshake and authentication"));
    let expected_block = "\
Host *
    ControlMaster auto
    ControlPath ~/.ssh/cm-%r@%h:%p
    ControlPersist 10m";
    assert!(
        msg.contains(expected_block),
        "warning message must contain exact config block:\n{msg}"
    );
}
