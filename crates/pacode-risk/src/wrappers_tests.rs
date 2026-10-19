use super::*;

fn words(source: &str) -> Vec<Token> {
    crate::tokenize::tokenize(source)
}

/// What `source`'s wrapper runs, as text.
fn inner(source: &str) -> String {
    let words = words(source);
    let Some(unwrapped) = unwrap(&words) else {
        return "not a wrapper".to_string();
    };
    match unwrapped.inner {
        Inner::Argv(rest) => rest
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        Inner::Scripts(scripts) => format!("script: {}", scripts.join(" | ")),
        Inner::Nothing => "nothing".to_string(),
        Inner::Opaque(rest) => format!("opaque: {}", rest.len()),
        Inner::Unknown(_) => "unknown".to_string(),
    }
}

#[test]
fn options_with_values_are_skipped_with_their_value() {
    for (source, expected) in [
        ("sudo -u root -g wheel cmd a", "cmd a"),
        ("sudo --user root cmd", "cmd"),
        ("sudo --user=root -E -- cmd", "cmd"),
        ("env -u NAME -C /tmp -i A=1 B=2 cmd", "cmd"),
        ("env - A=1 cmd", "cmd"),
        ("nice -n 5 cmd", "cmd"),
        ("nice -10 cmd", "cmd"),
        ("timeout -s KILL -k 1 5 cmd", "cmd"),
        ("timeout --preserve-status 5s cmd", "cmd"),
        ("xargs -a list -I{} -n1 -0 cmd {}", "cmd {}"),
        ("xargs -i{} cmd", "cmd"),
        ("stdbuf -o L -eL cmd", "cmd"),
        ("chroot /new cmd", "cmd"),
        ("time -p -o out cmd", "cmd"),
        ("exec -a name cmd", "cmd"),
        ("busybox rm x", "rm x"),
    ] {
        assert_eq!(inner(source), expected, "{source:?}");
    }
}

#[test]
fn some_wrappers_run_a_command_line_or_nothing() {
    for (source, expected) in [
        ("eval rm -rf x", "script: rm -rf x"),
        ("watch -n 1 'ls; rm x'", "script: ls; rm x"),
        ("trap 'rm x' EXIT", "script: rm x"),
        ("trap - EXIT", "nothing"),
        ("su root -c 'rm x'", "script: rm x"),
        ("su --command='rm x'", "script: rm x"),
        ("su -lc 'rm x'", "script: rm x"),
        ("su", "nothing"),
        ("env -S 'rm x' y", "script: rm x y"),
        ("bash -lc 'rm x'", "script: rm x"),
        ("command -v cargo", "nothing"),
        ("env", "nothing"),
        ("strace -f cmd", "opaque: 2"),
        ("ls -la", "not a wrapper"),
    ] {
        assert_eq!(inner(source), expected, "{source:?}");
    }
}

#[test]
fn an_unknown_option_is_not_guessed() {
    for source in [
        "sudo --frobnicate cmd",
        "env --un=X cmd",
        "timeout -q 5 cmd",
        "xargs -Q cmd",
    ] {
        assert_eq!(inner(source), "unknown", "{source:?}");
    }
}

#[test]
fn time_reports_the_file_it_writes() {
    for source in ["time -o out ls", "time --output=out ls", "time -aoout ls"] {
        let words = words(source);
        let unwrapped = unwrap(&words).unwrap();
        let written: Vec<_> = unwrapped.writes.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(written, ["out"], "{source:?}");
    }
}
