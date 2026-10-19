use super::*;
use crate::syntax::{Lang, Lexer};

fn parts(lang: Lang, line: &str) -> Vec<(Option<TokenKind>, &str)> {
    let mut lx = Lexer::for_lang(lang);
    lx.lex_line(line)
        .iter()
        .map(|t| (t.kind, &line[t.range.clone()]))
        .collect()
}

fn has(parts: &[(Option<TokenKind>, &str)], kind: TokenKind, text: &str) -> bool {
    parts.iter().any(|(k, s)| *k == Some(kind) && *s == text)
}

#[test]
fn toml_sample() {
    let p = parts(Lang::Toml, "[package]\n");
    assert!(has(&p, TokenKind::Type, "[package]"));
    let p = parts(Lang::Toml, "name = \"pacode\" # comment");
    assert!(has(&p, TokenKind::Function, "name"));
    assert!(has(&p, TokenKind::Operator, "="));
    assert!(has(&p, TokenKind::String, "\"pacode\""));
    assert!(has(&p, TokenKind::Comment, "# comment"));
    let mut lx = Lexer::for_lang(Lang::Toml);
    let t2 = lx.lex_line("date = 1979-05-27T07:32:00Z");
    assert!(t2.iter().any(|t| t.kind == Some(TokenKind::Number)
        && &"date = 1979-05-27T07:32:00Z"[t.range.clone()] == "1979-05-27T07:32:00Z"));
    let t3 = lx.lex_line("ok = true");
    assert!(t3.iter().any(|t| t.kind == Some(TokenKind::Keyword)));
}

#[test]
fn yaml_sample() {
    let p = parts(Lang::Yaml, "- name: test # c");
    assert!(has(&p, TokenKind::Operator, "-"));
    assert!(has(&p, TokenKind::Function, "name"));
    assert!(has(&p, TokenKind::Comment, "# c"));
    let p = parts(Lang::Yaml, "key: [1, on]");
    assert!(has(&p, TokenKind::Function, "key"));
    assert!(has(&p, TokenKind::Keyword, "on"));
    // `#` mid-word is not a comment in YAML either.
    let p = parts(Lang::Yaml, "a: b#c");
    assert!(!p.iter().any(|(k, _)| *k == Some(TokenKind::Comment)));
}

#[test]
fn yaml_block_scalar_owns_indented_lines() {
    let mut lx = Lexer::for_lang(Lang::Yaml);
    lx.lex_line("script: |");
    assert!(lx.in_block());
    let toks = lx.lex_line("  echo hi # not a comment");
    assert!(lx.in_block());
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, Some(TokenKind::String));
    let toks = lx.lex_line("next: 1");
    assert!(!lx.in_block());
    assert!(toks.iter().any(|t| t.kind == Some(TokenKind::Function)));
}

#[test]
fn bash_sample() {
    let p = parts(Lang::Bash, "if [ -f f ]; then echo $X | grep -q x # c");
    assert!(has(&p, TokenKind::Keyword, "if"));
    assert!(has(&p, TokenKind::Keyword, "then"));
    assert!(has(&p, TokenKind::Function, "echo"));
    assert!(has(&p, TokenKind::Type, "$X"));
    assert!(has(&p, TokenKind::Operator, "|"));
    assert!(has(&p, TokenKind::Keyword, "-q"));
    assert!(has(&p, TokenKind::Comment, "# c"));
}

#[test]
fn bash_assignment_is_not_a_command() {
    let p = parts(Lang::Bash, "FOO=bar");
    assert!(has(&p, TokenKind::Type, "FOO"));
    assert!(!p.iter().any(|(k, _)| *k == Some(TokenKind::Function)));
    let p = parts(Lang::Bash, "ls -la # tail");
    assert!(has(&p, TokenKind::Function, "ls"));
    assert!(has(&p, TokenKind::Keyword, "-la"));
}

#[test]
fn markdown_sample() {
    let p = parts(Lang::Markdown, "# Title");
    assert!(has(&p, TokenKind::Keyword, "#"));
    assert!(has(&p, TokenKind::Type, " Title"));
    let p = parts(Lang::Markdown, "- use `code` and **bold** and [t](u)");
    assert!(has(&p, TokenKind::Operator, "-"));
    assert!(has(&p, TokenKind::String, "`code`"));
    assert!(has(&p, TokenKind::Type, "**bold**"));
    assert!(has(&p, TokenKind::Function, "[t]"));
    assert!(has(&p, TokenKind::String, "(u)"));
}
