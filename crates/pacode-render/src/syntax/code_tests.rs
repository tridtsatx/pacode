use crate::syntax::{Lexer, TokenKind};

fn parts(lang: crate::syntax::Lang, line: &str) -> Vec<(Option<TokenKind>, &str)> {
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
fn rust_sample() {
    let p = parts(
        crate::syntax::Lang::Rust,
        "fn main() -> i32 { let x: Vec<u8> = vec![1]; // go",
    );
    assert!(has(&p, TokenKind::Keyword, "fn"));
    assert!(has(&p, TokenKind::Function, "main"));
    assert!(has(&p, TokenKind::Type, "i32"));
    assert!(has(&p, TokenKind::Type, "Vec"));
    assert!(has(&p, TokenKind::Number, "1"));
    assert!(has(&p, TokenKind::Comment, "// go"));
    assert!(has(&p, TokenKind::Operator, "->"));
}

#[test]
fn rust_lifetime_vs_char() {
    let p = parts(crate::syntax::Lang::Rust, "fn f<'a>(c: char) { 'x' }");
    assert!(has(&p, TokenKind::Type, "'a"));
    assert!(has(&p, TokenKind::String, "'x'"));
}

#[test]
fn rust_raw_string() {
    let mut lx = Lexer::for_lang(crate::syntax::Lang::Rust);
    let line = "let s = r#\"a \"b\"#; x";
    let toks = lx.lex_line(line);
    // The raw string ends at `"#`, so the inner `"` does not close it.
    assert!(
        toks.iter()
            .any(|t| t.kind == Some(TokenKind::String) && &line[t.range.clone()] == "r#\"a \"b\"#")
    );
    assert!(
        toks.iter()
            .any(|t| t.kind == Some(TokenKind::Operator) && &line[t.range.clone()] == ";")
    );
}

#[test]
fn python_sample() {
    let p = parts(crate::syntax::Lang::Python, "def f(x: int) -> str:  # doc");
    assert!(has(&p, TokenKind::Keyword, "def"));
    assert!(has(&p, TokenKind::Function, "f"));
    assert!(has(&p, TokenKind::Type, "int"));
    assert!(has(&p, TokenKind::Type, "str"));
    assert!(has(&p, TokenKind::Comment, "# doc"));
}

#[test]
fn python_decorator_and_prefixed_string() {
    let p = parts(crate::syntax::Lang::Python, "@app.route('/x')");
    assert!(has(&p, TokenKind::Function, "@app"));
    let p = parts(crate::syntax::Lang::Python, "x = f\"{y}\"");
    assert!(has(&p, TokenKind::String, "f\"{y}\""));
}

#[test]
fn tsjs_sample() {
    let p = parts(
        crate::syntax::Lang::TypeScript,
        "const f = async (x: number): Promise<void> => x + 1; // c",
    );
    assert!(has(&p, TokenKind::Keyword, "const"));
    assert!(has(&p, TokenKind::Keyword, "async"));
    assert!(has(&p, TokenKind::Type, "number"));
    assert!(has(&p, TokenKind::Type, "Promise"));
    assert!(has(&p, TokenKind::Type, "void"));
    assert!(has(&p, TokenKind::Number, "1"));
    assert!(has(&p, TokenKind::Comment, "// c"));
}

#[test]
fn tsjs_template_string_spans_lines() {
    let mut lx = Lexer::for_lang(crate::syntax::Lang::TypeScript);
    let _toks = lx.lex_line("let s = `start ${x}");
    assert!(lx.in_block());
    let toks = lx.lex_line("end` + y");
    assert!(!lx.in_block());
    assert_eq!(toks[0].kind, Some(TokenKind::String));
}

#[test]
fn json_sample() {
    let p = parts(
        crate::syntax::Lang::Json,
        "{\"key\": [1, true, null, -2.5]}",
    );
    assert!(has(&p, TokenKind::String, "\"key\""));
    assert!(has(&p, TokenKind::Keyword, "true"));
    assert!(has(&p, TokenKind::Keyword, "null"));
    assert!(has(&p, TokenKind::Number, "1"));
    assert!(has(&p, TokenKind::Operator, ":"));
    assert!(has(&p, TokenKind::Operator, "{"));
}
