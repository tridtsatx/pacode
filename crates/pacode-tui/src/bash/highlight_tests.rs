use super::*;

#[test]
fn test_tokenize_simple_command_and_flags() {
    let input = "ls -la /tmp";
    let spans = tokenize(input);
    assert_eq!(
        spans,
        vec![
            BashSpan {
                text: "ls",
                role: BashRole::Command
            },
            BashSpan {
                text: " ",
                role: BashRole::Argument
            },
            BashSpan {
                text: "-la",
                role: BashRole::Flag
            },
            BashSpan {
                text: " ",
                role: BashRole::Argument
            },
            BashSpan {
                text: "/tmp",
                role: BashRole::Argument
            },
        ]
    );
    let reconstructed: String = spans.iter().map(|s| s.text).collect();
    assert_eq!(reconstructed, input);
}

#[test]
fn test_tokenize_pipes_and_command_chains() {
    let input = "cat file.txt | grep error && echo done || exit 1 ; rm tmp";
    let spans = tokenize(input);
    let commands: Vec<&str> = spans
        .iter()
        .filter(|s| s.role == BashRole::Command)
        .map(|s| s.text)
        .collect();
    assert_eq!(commands, vec!["cat", "grep", "echo", "exit", "rm"]);

    let operators: Vec<&str> = spans
        .iter()
        .filter(|s| s.role == BashRole::Operator)
        .map(|s| s.text)
        .collect();
    assert_eq!(operators, vec!["|", "&&", "||", ";"]);

    let reconstructed: String = spans.iter().map(|s| s.text).collect();
    assert_eq!(reconstructed, input);
}

#[test]
fn test_tokenize_quoted_strings_single_and_double() {
    let input = "echo 'single quote' \"double quote\"";
    let spans = tokenize(input);
    let strings: Vec<&str> = spans
        .iter()
        .filter(|s| s.role == BashRole::String)
        .map(|s| s.text)
        .collect();
    assert_eq!(strings, vec!["'single quote'", "\"double quote\""]);

    let reconstructed: String = spans.iter().map(|s| s.text).collect();
    assert_eq!(reconstructed, input);
}

#[test]
fn test_tokenize_unterminated_quote_to_end_of_line() {
    let input = "echo \"unterminated string";
    let spans = tokenize(input);
    assert_eq!(
        spans,
        vec![
            BashSpan {
                text: "echo",
                role: BashRole::Command
            },
            BashSpan {
                text: " ",
                role: BashRole::Argument
            },
            BashSpan {
                text: "\"unterminated string",
                role: BashRole::String
            },
        ]
    );

    let single_unterminated = "git commit -m 'half string";
    let spans2 = tokenize(single_unterminated);
    assert_eq!(
        spans2.last(),
        Some(&BashSpan {
            text: "'half string",
            role: BashRole::String
        })
    );
}

#[test]
fn test_tokenize_variables() {
    let input = "echo $USER ${HOME} $? $$";
    let spans = tokenize(input);
    let vars: Vec<&str> = spans
        .iter()
        .filter(|s| s.role == BashRole::Variable)
        .map(|s| s.text)
        .collect();
    assert_eq!(vars, vec!["$USER", "${HOME}", "$?", "$$"]);

    let reconstructed: String = spans.iter().map(|s| s.text).collect();
    assert_eq!(reconstructed, input);
}

#[test]
fn test_tokenize_redirections() {
    let input = "cmd < input.txt > output.txt >> append.log 2>&1 &";
    let spans = tokenize(input);
    let ops: Vec<&str> = spans
        .iter()
        .filter(|s| s.role == BashRole::Operator)
        .map(|s| s.text)
        .collect();
    assert_eq!(ops, vec!["<", ">", ">>", "2>&1", "&"]);

    let reconstructed: String = spans.iter().map(|s| s.text).collect();
    assert_eq!(reconstructed, input);
}

/// Every input must be reconstructible byte-for-byte from non-empty spans. An
/// empty span means the scanner stood still on that byte — the shape that made
/// `$(` spin the tokenizer until it exhausted RAM.
fn assert_progress(input: &str) {
    let spans = tokenize(input);
    assert!(
        spans.iter().all(|s| !s.text.is_empty()),
        "empty span in {input:?}: {spans:?}"
    );
    let reconstructed: String = spans.iter().map(|s| s.text).collect();
    assert_eq!(reconstructed, input);
}

#[test]
fn test_tokenize_command_substitution_terminates() {
    // The real transcript line that grew the span vector past 3 GiB on redraw.
    assert_progress(
        "GIT_DIR=$(cd \"$(git rev-parse --git-dir)\" 2>/dev/null && pwd -P); \
         echo \"GIT_DIR=$GIT_DIR\"",
    );
    assert_progress("echo $(date)");
    assert_progress("$(cmd)");
    assert_progress("a=$(b $(c))");
}

#[test]
fn test_tokenize_bare_dollar_terminates() {
    assert_progress("echo $");
    assert_progress("$");
    assert_progress("echo $ $ $");
    assert_progress("cost is 5$ total");
    assert_progress("echo $-flag");
}

#[test]
fn test_tokenize_parens_are_operators() {
    assert_progress("(cd /tmp && ls)");
    let spans = tokenize("echo $(git status)");
    // The command inside the substitution is highlighted as a command, not as an
    // argument of `echo`.
    let commands: Vec<&str> = spans
        .iter()
        .filter(|s| s.role == BashRole::Command)
        .map(|s| s.text)
        .collect();
    assert_eq!(commands, vec!["echo", "git"]);
    let opener = spans.iter().find(|s| s.text == "$(").expect("$( span");
    assert_eq!(opener.role, BashRole::Variable);
}
