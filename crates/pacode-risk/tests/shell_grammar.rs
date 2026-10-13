//! The risk gate and the read-only check see every command a line runs,
//! wherever the shell grammar puts it: after `&`, inside `{ }`, behind `!`,
//! keywords, assignments, leading redirections, wrappers and their options,
//! substitutions inside double quotes and unquoted here-documents.
//!
//! Each row names a command line, the risk level it must get, and whether it
//! may pass as read-only. Destructive rows target `~` so the level is the one
//! the hidden command would get on its own.

use std::path::Path;

use pacode_risk::{RiskLevel, classify_with_home, is_read_only};

use RiskLevel::{Catastrophic, Confirm, Low, Safe};

fn level(command: &str) -> RiskLevel {
    classify_with_home(
        command,
        Path::new("/home/tester/proj"),
        Path::new("/home/tester"),
    )
    .level
}

/// `(command, risk level, read-only)`.
type Row = (&'static str, RiskLevel, bool);

fn check(rows: &[Row]) {
    let mut failures = Vec::new();
    for &(command, want_level, want_read_only) in rows {
        let got_level = level(command);
        let got_read_only = is_read_only(command);
        if got_level != want_level || got_read_only != want_read_only {
            failures.push(format!(
                "{command:?}: level {got_level:?} (want {want_level:?}), \
                 read-only {got_read_only} (want {want_read_only})"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Rows whose level only has to reach the confirmation floor: the parser
/// cannot tell what runs, so it must not call the line `Safe`.
fn check_at_least_confirm(commands: &[&str]) {
    let mut failures = Vec::new();
    for &command in commands {
        let got = level(command);
        if got < Confirm || is_read_only(command) {
            failures.push(format!(
                "{command:?}: level {got:?}, read-only {}",
                is_read_only(command)
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn background_and_pipe_operators_separate_commands() {
    check(&[
        ("ls & rm -rf ~", Catastrophic, false),
        ("ls &rm -rf ~", Catastrophic, false),
        ("rm -rf ~ &", Catastrophic, false),
        ("ls |& rm -rf ~", Catastrophic, false),
        ("ls & touch x", Safe, false),
        ("git status & git commit -am x", Safe, false),
        ("ls & ls", Safe, true),
        ("ls |& grep x", Safe, true),
        ("ls; rm -rf ~", Catastrophic, false),
        ("ls\nrm -rf ~", Catastrophic, false),
    ]);
}

#[test]
fn grouping_negation_and_keywords_are_not_the_program() {
    check(&[
        ("{ rm -rf ~; }", Catastrophic, false),
        ("{ ls; }", Safe, true),
        ("{ ls; } > /dev/null", Safe, true),
        ("! rm -rf ~", Catastrophic, false),
        ("! ls", Safe, true),
        ("( rm -rf ~ )", Catastrophic, false),
        ("if true; then rm -rf ~; fi", Catastrophic, false),
        ("if ls; then cat x; else echo no; fi", Safe, true),
        ("while true; do rm -rf ~; done", Catastrophic, false),
        ("until false; do ls; done", Safe, true),
        ("for f in a b; do rm -rf ~; done", Catastrophic, false),
        ("for f in *.rs; do wc -l \"$f\"; done", Safe, true),
        ("for x do rm -rf ~; done", Catastrophic, false),
        ("select x in a; do rm -rf ~; done", Catastrophic, false),
        ("case $x in a) rm -rf ~;; esac", Catastrophic, false),
        ("case $x in (a|b) rm -rf ~;; esac", Catastrophic, false),
        ("case $x in *) echo hi;; esac", Safe, true),
        (
            "case $x in a) ls;& b) rm -rf ~;;& esac",
            Catastrophic,
            false,
        ),
        ("f(){ rm -rf ~; }", Catastrophic, false),
        ("f() { ls; }; f", Safe, false),
        ("function f { rm -rf ~; }", Catastrophic, false),
        ("function f() { rm -rf ~; }", Catastrophic, false),
        ("coproc rm -rf ~", Catastrophic, false),
        ("coproc NAME { rm -rf ~; }", Catastrophic, false),
        ("[[ -f x ]] && rm -rf ~", Catastrophic, false),
        ("[[ $a > $b ]] && echo y", Safe, true),
        ("((x > 5)) && ls", Safe, true),
        // fish and zsh command prefixes
        ("true; and rm -rf ~", Catastrophic, false),
        ("false; or rm -rf ~", Catastrophic, false),
        ("not rm -rf ~", Catastrophic, false),
        ("begin; rm -rf ~; end", Catastrophic, false),
        ("noglob rm -rf ~", Catastrophic, false),
        ("nocorrect rm -rf ~", Catastrophic, false),
        ("repeat 2 rm -rf ~", Catastrophic, false),
        ("switch $x; case a; rm -rf ~; end", Catastrophic, false),
    ]);
}

#[test]
fn assignments_and_leading_redirections_are_not_the_program() {
    check(&[
        ("X=1 rm -rf ~", Catastrophic, false),
        ("A=1 B=2 rm -rf ~", Catastrophic, false),
        (">/dev/null rm -rf ~", Catastrophic, false),
        ("2>/dev/null rm -rf ~", Catastrophic, false),
        ("</dev/null rm -rf ~", Catastrophic, false),
        // dash reads `&>` as `&` then `>`: the words after the target are a
        // command of their own there.
        ("ls &>/dev/null rm -rf ~", Catastrophic, false),
        ("X=1 ls", Safe, true),
        ("RUST_LOG=debug cargo test", Safe, false),
    ]);
}

#[test]
fn redirections_are_writes_not_separators() {
    check(&[
        ("ls &> out.txt", Low, false),
        ("ls &>> out.txt", Safe, false),
        ("ls >& out.txt", Low, false),
        ("ls >| out.txt", Low, false),
        ("ls 2>&1", Safe, true),
        ("ls >/dev/null 2>&1", Safe, true),
        ("ls &>/dev/null", Safe, true),
        ("ls 2>&- | head", Safe, true),
        ("cat <> f", Safe, false),
        ("cat <&3", Safe, true),
        ("echo x >> /dev/null", Safe, true),
        ("cat < ~/.ssh/id_rsa", Safe, false),
        ("ls &> .env", Catastrophic, false),
    ]);
}

#[test]
fn wrappers_with_options_still_expose_the_command() {
    check(&[
        ("time rm -rf ~", Catastrophic, false),
        ("time -p rm -rf ~", Catastrophic, false),
        ("time ls", Safe, true),
        ("time -o t.txt ls", Low, false),
        ("exec -a name rm -rf ~", Catastrophic, false),
        ("exec -c rm -rf ~", Catastrophic, false),
        ("eval 'rm -rf ~'", Catastrophic, false),
        ("eval \"rm -rf ~\"", Catastrophic, false),
        ("eval rm -rf '~'", Catastrophic, false),
        ("eval ls", Safe, false),
        ("command rm -rf ~", Catastrophic, false),
        ("command -p rm -rf ~", Catastrophic, false),
        ("command -v cargo", Safe, true),
        ("builtin eval 'rm -rf ~'", Catastrophic, false),
        ("nohup rm -rf ~", Catastrophic, false),
        ("env -u FOO rm -rf ~", Catastrophic, false),
        ("env --unset=FOO rm -rf ~", Catastrophic, false),
        ("env -i rm -rf ~", Catastrophic, false),
        ("env -C /tmp rm -rf ~", Catastrophic, false),
        ("env -S 'rm -rf ~'", Catastrophic, false),
        ("env -u cat rm x", Low, false),
        ("sudo -u root rm -rf ~", Catastrophic, false),
        ("sudo -E rm -rf ~", Catastrophic, false),
        ("sudo --user=root rm -rf ~", Catastrophic, false),
        ("sudo --user root rm -rf ~", Catastrophic, false),
        ("sudo -g wheel rm -rf ~", Catastrophic, false),
        ("sudo -- rm -rf ~", Catastrophic, false),
        ("doas -u root rm -rf ~", Catastrophic, false),
        ("nice -n 5 rm -rf ~", Catastrophic, false),
        ("nice --adjustment=5 rm -rf ~", Catastrophic, false),
        ("timeout -s KILL 5 rm -rf ~", Catastrophic, false),
        ("timeout --kill-after=1 5 rm -rf ~", Catastrophic, false),
        ("stdbuf -o L rm -rf ~", Catastrophic, false),
        ("ionice -c 3 rm -rf ~", Catastrophic, false),
        ("setsid -w rm -rf ~", Catastrophic, false),
        ("su -c 'rm -rf ~'", Catastrophic, false),
        ("su root -c 'rm -rf ~'", Catastrophic, false),
        ("su --command='rm -rf ~' root", Catastrophic, false),
        ("watch 'rm -rf ~'", Catastrophic, false),
        ("watch -n 1 rm -rf ~", Catastrophic, false),
        ("trap 'rm -rf ~' EXIT", Catastrophic, false),
        ("busybox rm -rf ~", Catastrophic, false),
        ("find . -exec sh -c 'rm -rf ~' \\;", Catastrophic, false),
        ("fd -x sh -c 'rm -rf ~'", Catastrophic, false),
        ("xargs -a list.txt rm -rf", Confirm, false),
        ("xargs --arg-file=list.txt rm -rf", Confirm, false),
        ("xargs -I{} rm -rf {}", Confirm, false),
        ("time cargo build", Safe, false),
        ("timeout 5 cat file.txt", Safe, true),
        ("timeout --kill-after 1 5 ls", Safe, true),
        ("nice -n 5 grep x f", Safe, true),
        ("find . -type f | xargs grep TODO", Safe, true),
    ]);
}

#[test]
fn wrapper_options_the_table_does_not_know_are_not_trusted() {
    check_at_least_confirm(&[
        "timeout --frobnicate 5 rm x",
        "env --frobnicate rm x",
        "xargs --frobnicate ls",
        "nice --frobnicate ls",
        "sudo --frobnicate ls",
    ]);
}

#[test]
fn substitutions_are_commands_wherever_they_appear() {
    check(&[
        ("echo \"$(rm -rf ~)\"", Catastrophic, false),
        ("echo \"`rm -rf ~`\"", Catastrophic, false),
        ("echo \"${x:-$(rm -rf ~)}\"", Catastrophic, false),
        ("echo ${x:-$(rm -rf ~)}", Catastrophic, false),
        ("cat <(rm -rf ~)", Catastrophic, false),
        ("tee >(rm -rf ~)", Catastrophic, false),
        ("diff <(ls) <(ls -a)", Safe, false),
        ("rm $(echo a) -rf ~", Catastrophic, false),
        ("echo $(( $(rm -rf ~) ))", Catastrophic, false),
        ("echo $((1 + 2))", Safe, true),
        ("echo $((x > 5))", Safe, true),
        ("x=$(rm -rf ~)", Catastrophic, false),
        ("export X=\"$(rm -rf ~)\"", Catastrophic, false),
        ("git commit -m \"$(cat msg.txt)\"", Safe, false),
        ("echo '$(rm -rf ~)'", Safe, true),
        ("echo (rm -rf ~)", Catastrophic, false),
    ]);
}

#[test]
fn here_documents_expand_unless_the_delimiter_is_quoted() {
    check(&[
        ("cat <<EOF\n$(rm -rf ~)\nEOF", Catastrophic, false),
        ("cat <<-EOF\n\t`rm -rf ~`\n\tEOF", Catastrophic, false),
        ("cat <<'EOF'\n$(rm -rf ~)\nEOF", Safe, true),
        ("cat <<\"EOF\"\n$(rm -rf ~)\nEOF", Safe, true),
        ("cat <<\\EOF\n$(rm -rf ~)\nEOF", Safe, true),
        ("cat <<EOF\nplain text\nEOF", Safe, true),
        ("cat <<EOF\nrm -rf ~\nEOF", Safe, true),
        ("cat <<EOF\ninert\nEOF\nrm -rf ~", Catastrophic, false),
        ("cat <<<\"$(rm -rf ~)\"", Catastrophic, false),
        ("cat <<< hello", Safe, true),
    ]);
}

#[test]
fn words_the_shell_rewrites_are_resolved_or_distrusted() {
    check(&[
        ("{rm,-rf,~}", Catastrophic, false),
        ("rm -rf {~,/tmp/x}", Catastrophic, false),
        ("r\\m -rf ~", Catastrophic, false),
        ("\"rm\" -rf ~", Catastrophic, false),
        ("r''m -rf ~", Catastrophic, false),
        ("rm -rf \\\n~", Catastrophic, false),
        ("ls # ; rm -rf ~", Safe, true),
        ("ls a#b; rm -rf ~", Catastrophic, false),
    ]);
    check_at_least_confirm(&[
        "$CMD -rf ~",
        "${RM:-rm} -rf ~",
        "/???/r? -rf ~",
        "/bin/r[m] -rf ~",
        "$'\\x72m' -rf ~",
        "=rm -rf ~",
        "\"$(echo rm)\" -rf ~",
        "ls *(e:'rm -rf ~':)",
        "echo 'unterminated",
        "echo \"unterminated",
        "echo $(case x in a) rm -rf x;; esac)",
    ]);
}

#[test]
fn ordinary_command_lines_keep_their_verdicts() {
    check(&[
        ("cargo build 2>&1 | tail -20", Safe, false),
        ("git log --oneline | head -5", Safe, true),
        ("cd crates && cargo test", Safe, false),
        ("[ -f x ] && cat x", Safe, true),
        ("test -f Cargo.toml && cat Cargo.toml", Safe, true),
        ("rg foo src || true", Safe, true),
        ("cat a; cat b", Safe, true),
        ("ls 2>/dev/null", Safe, true),
        ("echo \"$HOME\"", Safe, true),
        ("type rm", Safe, true),
        ("which rm", Safe, true),
        ("rm -rf target", Low, false),
        ("find . -name '*.rs' -exec grep -l foo {} +", Low, false),
        (
            "printf '%s\\n' /home/tester/.ssh/id_ed25519 >/tmp/list",
            Safe,
            false,
        ),
        (
            "git commit -m \"$(cat <<'EOF'\nfix: don't drop the (last) row\n\nEOF\n)\"",
            Safe,
            false,
        ),
        ("echo $(ls # it's a comment\n)", Safe, false),
        ("cd crates/x && cargo test 2>&1 | tail -30", Safe, false),
        ("ls -la; echo \"---\"; cat Cargo.toml", Safe, true),
        ("grep -rn \"foo\" --include=*.rs src | head", Safe, true),
        (
            "find . -name '*.rs' | xargs wc -l | sort -n | tail",
            Safe,
            true,
        ),
        (
            "python3 -c \"import json; print(json.dumps({'a': 1}))\"",
            Safe,
            false,
        ),
        ("echo \"a (b) c\"", Safe, true),
        ("awk -F'(' '{print $1}' f", Safe, true),
        ("cat > notes.md <<'EOF'\n# Title\nEOF", Low, false),
        ("diff -u a b || true", Safe, true),
        ("test -d target && du -sh target", Safe, true),
        ("jq '.items[] | select(.x > 1)' f.json", Safe, true),
        ("rg 'fn \\w+\\(' src", Safe, true),
        ("git log --format='%H %s' -n 5", Safe, true),
        ("wc -l $(git ls-files)", Safe, false),
        ("ls -d */", Safe, true),
        ("echo {a,b}", Safe, true),
        ("sleep 1 && echo done", Safe, false),
        ("make -j8 2>&1 | tail", Safe, false),
        ("npm test -- --watch=false", Safe, false),
        (
            "x=$(cat <<EOF\nit's $(rm -rf ~)\nEOF\n)",
            Catastrophic,
            false,
        ),
    ]);
}

