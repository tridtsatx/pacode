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
