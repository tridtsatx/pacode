use super::*;

#[test]
fn heredoc_payload_is_data_not_shell_source() {
    let command = "cat > /tmp/repro.sh <<'EOF'\n#!/bin/bash\nrm -rf \"$SOME_VAR\"\nEOF\necho wrote";
    let segments = split_segments(command);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0][0].text, "cat");
    assert_eq!(segments[1][0].text, "echo");
    assert!(!segments.iter().flatten().any(|token| token.text == "rm"));
}

#[test]
fn heredoc_terminator_does_not_hide_following_command() {
    let command = "cat <<EOF\ninert\nEOF\nrm -rf ~";
    let segments = split_segments(command);
    assert!(segments.iter().any(|segment| segment[0].text == "rm"));
}

#[test]
fn tab_stripping_and_escaped_delimiters_are_supported() {
    for command in [
        "cat <<-EOF\n\trm -rf ~\n\tEOF\necho safe",
        "cat <<\\EOF\nrm -rf ~\nEOF\necho safe",
    ] {
        let segments = split_segments(command);
        assert!(!segments.iter().flatten().any(|token| token.text == "rm"));
        assert!(segments.iter().any(|segment| segment[0].text == "echo"));
    }
}

fn texts(command: &str) -> Vec<String> {
    tokenize(command).into_iter().map(|t| t.text).collect()
}

#[test]
fn quoting_does_not_hide_a_target() {
    for command in [r#"rm -rf $HOME"#, r#"rm -rf "$HOME""#, r#"rm -rf '$HOME'"#] {
        assert_eq!(texts(command), vec!["rm", "-rf", "$HOME"], "{command}");
    }
}

#[test]
fn backslash_escapes_are_resolved() {
    assert_eq!(
        texts(r"rm -rf /home/my\ files"),
        vec!["rm", "-rf", "/home/my files"]
    );
}

#[test]
fn chained_commands_split_into_separate_segments() {
    let segments = split_segments("echo hi && rm -rf /tmp/a; rm -rf /tmp/b");
    assert_eq!(segments.len(), 3, "{segments:#?}");
    assert_eq!(segments[1][0].text, "rm");
    assert_eq!(segments[2][0].text, "rm");
}

#[test]
fn piped_commands_are_separate_segments() {
    let segments = split_segments("find . -type f | xargs rm -rf");
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[1][0].text, "xargs");
}

#[test]
fn fd_redirection_tokenization() {
    let tokens = tokenize("cmd 2>&1");
    assert!(tokens.iter().any(|t| t.text == "2>&1" && t.is_operator));

    let tokens = tokenize("find / 2>/dev/null");
    assert!(tokens.iter().any(|t| t.text == "2>" && t.is_operator));
    assert!(
        tokens
            .iter()
            .any(|t| t.text == "/dev/null" && !t.is_operator)
    );
    assert!(!tokens.iter().any(|t| t.text == "2" && !t.is_operator));
}

#[test]
fn background_and_both_stream_operators_are_their_own_tokens() {
    let ops = |command: &str| -> Vec<String> {
        tokenize(command)
            .into_iter()
            .filter(|t| t.is_operator)
            .map(|t| t.text)
            .collect()
    };
    assert_eq!(ops("a & b"), ["&"]);
    assert_eq!(ops("a |& b"), ["|&"]);
    assert_eq!(ops("a &> f"), ["&>"]);
    assert_eq!(ops("a &>> f"), ["&>>"]);
    assert_eq!(ops("a >& f"), [">&"]);
    assert_eq!(ops("a >&2"), [">&2"]);
    assert_eq!(ops("a <> f"), ["<>"]);
    assert_eq!(ops("a <&-"), ["<&-"]);
    assert_eq!(ops("a ;; b ;& c ;;& d"), [";;", ";&", ";;&"]);
}

#[test]
fn substitutions_keep_their_text_and_hand_back_their_source() {
    let lexed = lex("echo \"a$(rm -rf ~)b\" `id` ${x:-$(pwd)} <(ls) 'q$(no)'");
    let words: Vec<_> = lexed.tokens.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(
        words,
        [
            "echo",
            "a$(rm -rf ~)b",
            "`id`",
            "${x:-$(pwd)}",
            "<(ls)",
            "q$(no)"
        ]
    );
    assert_eq!(lexed.nested, ["rm -rf ~", "id", "pwd", "ls"]);
    assert!(lexed.unresolved.is_empty());
}

#[test]
fn comments_and_line_continuations_follow_the_shell() {
    assert_eq!(texts("ls # rm -rf ~"), ["ls"]);
    assert_eq!(texts("ls a#b"), ["ls", "a#b"]);
    assert_eq!(texts("rm -rf \\\n~"), ["rm", "-rf", "~"]);
    assert_eq!(texts("echo \"a\\\nb\""), ["echo", "ab"]);
}

#[test]
fn arithmetic_is_one_word_and_only_its_substitutions_are_commands() {
    let lexed = lex("(( x > $(id) )) && echo $((1 > 2))");
    let words: Vec<_> = lexed.tokens.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(words, ["(( x > $(id) ))", "&&", "echo", "$((1 > 2))"]);
    assert_eq!(lexed.nested, ["id"]);
    // A `((` closed by `) )` is a subshell in a subshell.
    assert_eq!(lex("$((echo hi) )").nested, ["(echo hi) "]);
}

#[test]
fn unreadable_constructs_are_reported() {
    for (command, reason) in [
        ("echo 'open", UNTERMINATED),
        ("echo \"open", UNTERMINATED),
        ("echo $(open", UNTERMINATED),
        ("echo `open", UNTERMINATED),
        ("ls *(e:x:)", GLUED_PAREN),
        ("echo $(case x in a) ls;; esac)", CASE_IN_SUBSTITUTION),
    ] {
        assert!(
            lex(command).unresolved.contains(&reason),
            "{command:?} must report {reason:?}"
        );
    }
    assert!(lex("f() { ls; }").unresolved.is_empty());
}

#[test]
fn recursive_flag_detected_in_bundles_and_long_form() {
    let cases = [
        ("-rf", true),
        ("-fr", true),
        ("-R", true),
        ("--recursive", true),
        ("-f", false),
        ("--force", false),
        ("-i", false),
    ];
    for (flag, expected) in cases {
        let token = Token::word(flag);
        assert_eq!(token.is_recursive_flag(), expected, "{flag}");
    }
}

#[test]
fn basename_strips_the_path_so_absolute_invocations_match() {
    assert_eq!(Token::word("/bin/rm").basename(), "rm");
    assert_eq!(Token::word("/usr/bin/shred").basename(), "shred");
}

#[test]
fn tokenizing_is_total_and_never_panics() {
    for command in [
        "",
        "   ",
        "'",
        "\"",
        "\\",
        ">",
        "&&",
        "rm -rf \"unterminated",
        "$(",
        "`",
        "${",
        "<<",
        "cat <<EOF",
        "((",
        "$((",
        "a <(",
        "f(",
        "\"$(\"",
        "case",
        "[[",
    ] {
        let _ = split_segments(command);
    }
}

#[test]
fn parentheses_split_subshell_and_command_substitution_bodies() {
    let segments = split_segments("x=$(rm -rf ~); (echo ok)");
    let texts: Vec<Vec<_>> = segments
        .into_iter()
        .map(|segment| segment.into_iter().map(|token| token.text).collect())
        .collect();
    assert_eq!(texts, vec![vec!["echo", "ok"], vec!["rm", "-rf", "~"]]);
}
