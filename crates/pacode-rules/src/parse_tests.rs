use std::path::PathBuf;

use pretty_assertions::assert_eq;

use crate::{Mode, Origin, RuleError, Scope, Severity, parse_rule};

fn project() -> Origin {
    Origin::Project(PathBuf::from(".pacode/rules/x.md"))
}

const FULL: &str = "---\n\
name: my-rule\n\
languages: [TS, .tsx]\n\
match: '\\bfoo\\(\\s*\"x\"'\n\
unless: 'ok'\n\
unless_above: '//\\s*OK'\n\
scope: any\n\
severity: block\n\
mode: stream\n\
max_per_turn: 5\n\
exclude: ['*.d.ts', tests/]\n\
skip_test_code: true\n\
---\n\
Do not call foo.\n\nSecond paragraph.\n";

#[test]
fn parses_full_rule() {
    let r = parse_rule(FULL, project()).expect("parse");
    assert_eq!(r.name, "my-rule");
    assert_eq!(r.languages, vec!["ts", "tsx"]);
    assert_eq!(r.pattern.as_str(), "\\bfoo\\(\\s*\"x\"");
    assert!(r.unless.is_some() && r.unless_above.is_some());
    assert_eq!(r.scope, Scope::Any);
    assert_eq!(r.severity, Severity::Block);
    assert_eq!(r.mode, Mode::Stream);
    assert_eq!(r.max_per_turn, 5);
    assert_eq!(r.exclude, vec!["*.d.ts", "tests/"]);
    assert!(r.skip_test_code);
    assert_eq!(r.message, "Do not call foo.\n\nSecond paragraph.");
}

#[test]
fn defaults_and_toml_style_and_dash_lists() {
    let src =
        "---\nname = \"a.b-c_1\"\n# comment\nlanguages:\n  - rs\n  - 'py'\nmatch = 'x'\n---\nmsg";
    let r = parse_rule(src, project()).expect("parse");
    assert_eq!(r.name, "a.b-c_1");
    assert_eq!(r.languages, vec!["rs", "py"]);
    assert_eq!(r.scope, Scope::Code);
    assert_eq!(r.severity, Severity::Warn);
    assert_eq!(r.mode, Mode::AfterWrite);
    assert_eq!(r.max_per_turn, 3);
    assert!(r.exclude.is_empty());
}

#[test]
fn crlf_and_bom() {
    let src = "\u{feff}---\r\nname: a\r\nlanguages: rs, ts\r\nmatch: 'x'\r\n---\r\nmsg\r\n";
    let r = parse_rule(src, project()).expect("parse");
    assert_eq!(r.languages, vec!["rs", "ts"]);
    assert_eq!(r.message, "msg");
}

#[test]
fn errors() {
    let no_front = parse_rule("just text", project());
    assert!(matches!(no_front, Err(RuleError::Frontmatter { .. })));
    let unterminated = parse_rule("---\nname: a\n", project());
    assert!(matches!(unterminated, Err(RuleError::Frontmatter { .. })));

    let base =
        |extra: &str| format!("---\nname: a\nlanguages: [rs]\nmatch: 'x'\n{extra}\n---\nmsg");
    assert!(parse_rule(&base(""), project()).is_ok());
    for bad in [
        "severity: loud",
        "mode: sometimes",
        "scope: all",
        "max_per_turn: 0",
        "max_per_turn: x",
        "skip_test_code: maybe",
    ] {
        assert!(
            matches!(
                parse_rule(&base(bad), project()),
                Err(RuleError::BadValue { .. })
            ),
            "{bad}"
        );
    }
    assert!(matches!(
        parse_rule(&base("unless: '('"), project()),
        Err(RuleError::BadRegex {
            field: "unless",
            ..
        })
    ));
    assert!(matches!(
        parse_rule(
            "---\nname: A B\nlanguages: [rs]\nmatch: 'x'\n---\nmsg",
            project()
        ),
        Err(RuleError::BadValue { field: "name", .. })
    ));
    assert!(matches!(
        parse_rule("---\nname: a\nlanguages: [rs]\n---\nmsg", project()),
        Err(RuleError::MissingField { field: "match", .. })
    ));
    assert!(matches!(
        parse_rule("---\nname: a\nmatch: 'x'\n---\nmsg", project()),
        Err(RuleError::MissingField {
            field: "languages",
            ..
        })
    ));
    assert!(matches!(
        parse_rule(
            "---\nname: a\nlanguages: [rs]\nmatch: 'x'\n---\n   \n",
            project()
        ),
        Err(RuleError::MissingField { field: "body", .. })
    ));
}
