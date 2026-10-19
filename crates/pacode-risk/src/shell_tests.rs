use super::*;

/// The words of each command, as text.
fn commands(source: &str) -> Vec<Vec<String>> {
    parse(source)
        .commands
        .into_iter()
        .map(|c| c.words.into_iter().map(|t| t.text).collect())
        .collect()
}

#[test]
fn grammar_around_a_command_is_not_a_command() {
    for (source, expected) in [
        ("{ rm x; }", vec![vec!["rm", "x"]]),
        ("! rm x", vec![vec!["rm", "x"]]),
        (
            "if a; then b; else c; fi",
            vec![vec!["a"], vec!["b"], vec!["c"]],
        ),
        ("for f in a b; do rm $f; done", vec![vec!["rm", "$f"]]),
        ("for f do rm $f; done", vec![vec!["rm", "$f"]]),
        (
            "case $x in a|b) one;; (c) two;& *) three;; esac",
            vec![vec!["one"], vec!["two"], vec!["three"]],
        ),
        ("f() { body; }", vec![vec!["body"]]),
        ("function f { body; }", vec![vec!["body"]]),
        ("coproc NAME { body; }", vec![vec!["body"]]),
        ("coproc body arg", vec![vec!["body", "arg"]]),
        ("[[ $a > b && -f c ]] && ok", vec![vec!["ok"]]),
        ("(( i > 2 )) || ok", vec![vec!["ok"]]),
        ("X=1 Y=2 run it", vec![vec!["run", "it"]]),
        ("> out 2>&1 run", vec![vec!["run"]]),
        ("true; and run", vec![vec!["true"], vec!["run"]]),
    ] {
        assert_eq!(commands(source), expected, "{source:?}");
    }
}

#[test]
fn redirections_carry_their_targets_and_kind() {
    let script = parse("cmd > a >> b 2>&1 &> c < d <> e");
    let command = &script.commands[0];
    let summary: Vec<_> = command
        .redirects
        .iter()
        .map(|r| {
            (
                r.op.as_str(),
                r.target.as_ref().map(|t| t.text.as_str()),
                r.writes(),
                r.truncates(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (">", Some("a"), true, true),
            (">>", Some("b"), true, false),
            ("2>&1", None, false, false),
            ("&>", Some("c"), true, true),
            ("<", Some("d"), false, false),
            ("<>", Some("e"), true, false),
        ]
    );
    assert_eq!(command.words.len(), 1);
}

#[test]
fn pipes_mark_the_receiving_command() {
    let script = parse("a | b |& c; d");
    let piped: Vec<bool> = script.commands.iter().map(|c| c.receives_pipe).collect();
    assert_eq!(piped, [false, true, true, false]);
}

#[test]
fn dash_reading_of_both_streams_redirection_adds_a_command() {
    assert_eq!(
        commands("ls &>/dev/null X=1 rm -rf x"),
        vec![vec!["rm", "-rf", "x"], vec!["ls", "X=1", "rm", "-rf", "x"]]
    );
    assert_eq!(commands("ls &>/dev/null"), vec![vec!["ls"]]);
}

#[test]
fn substitutions_are_counted_and_parsed() {
    let script = parse("echo \"$(a)\" `b` <(c)");
    assert_eq!(script.substitutions, 3);
    assert_eq!(
        commands("echo \"$(a)\" `b` <(c)"),
        vec![
            vec!["echo", "$(a)", "`b`", "<(c)"],
            vec!["a"],
            vec!["b"],
            vec!["c"]
        ]
    );
}

#[test]
fn deep_nesting_and_open_grammar_are_unresolved() {
    let deep = format!("{}x{}", "$(".repeat(12), ")".repeat(12));
    assert!(!parse(&deep).unresolved.is_empty());
    assert!(!parse("case x in a) ls;;").unresolved.is_empty());
    assert!(!parse("[[ -f x ; rm y").unresolved.is_empty());
    assert!(parse("case x in a) ls;; esac").unresolved.is_empty());
}

#[test]
fn assignments_are_recognised_by_their_name() {
    for word in ["X=1", "_x=", "A_1+=2", "arr[3]=v"] {
        assert!(is_assignment(word), "{word}");
    }
    for word in ["=x", "1X=2", "--opt=v", "a-b=c", "x"] {
        assert!(!is_assignment(word), "{word}");
    }
}

#[test]
fn brace_expansion_lists_alternatives_and_ranges() {
    assert_eq!(expand_braces("{rm,-rf,~}", 64).unwrap(), ["rm", "-rf", "~"]);
    assert_eq!(expand_braces("a{1..3}", 64).unwrap(), ["a1", "a2", "a3"]);
    assert_eq!(
        expand_braces("{a,b}{x,y}", 64).unwrap(),
        ["ax", "ay", "bx", "by"]
    );
    assert_eq!(expand_braces("x{a,{b,c}}", 64).unwrap(), ["xa", "xb", "xc"]);
    for literal in ["{}", "{a}", "${x}", "plain"] {
        assert_eq!(expand_braces(literal, 64).unwrap(), [literal]);
    }
    assert!(expand_braces("{1..5000}", 64).is_none());
    assert!(expand_braces("{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}", 64).is_none());
}
