use super::*;
use crate::syntax::Lang;

fn covered(lx: &mut Lexer, line: &str) -> Vec<Token> {
    let toks = lx.lex_line(line);
    // The tile property: tokens partition the line, contiguous and complete.
    let mut at = 0;
    for t in &toks {
        assert_eq!(t.range.start, at, "gap before token in {line:?}");
        at = t.range.end;
    }
    assert_eq!(at, line.len(), "tokens do not cover {line:?}");
    toks
}

#[test]
fn tokens_tile_the_whole_line() {
    for lang in [
        Lang::Rust,
        Lang::Python,
        Lang::TypeScript,
        Lang::Json,
        Lang::Toml,
        Lang::Yaml,
        Lang::Bash,
        Lang::Markdown,
    ] {
        let mut lx = Lexer::for_lang(lang);
        for line in [
            "",
            "   ",
            "fn main() { println!(\"hi\"); } // tail",
            "\u{0441}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} x = 42",
            "a /* b */ c",
            "unterminated \"str",
        ] {
            covered(&mut lx, line);
        }
    }
}

#[test]
fn block_state_carries_across_lines() {
    let mut lx = Lexer::for_lang(Lang::Rust);
    let toks = covered(&mut lx, "let s = \"start");
    assert!(lx.in_block());
    assert!(toks.iter().any(|t| t.kind == Some(TokenKind::String)));
    // The continuation line is one string token until the quote closes.
    let toks = covered(&mut lx, "middle \" + x");
    assert!(!lx.in_block());
    assert_eq!(toks[0].kind, Some(TokenKind::String));
}

#[test]
fn block_comment_carries_across_lines() {
    let mut lx = Lexer::for_lang(Lang::Python);
    covered(&mut lx, "x = \"\"\"doc");
    assert!(lx.in_block());
    let toks = covered(&mut lx, "still docs");
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, Some(TokenKind::String));
    covered(&mut lx, "end\"\"\" + y");
    assert!(!lx.in_block());
}

#[test]
fn line_over_the_cap_is_not_tokenized() {
    let mut lx = Lexer::for_lang(Lang::Rust);
    let long = "x".repeat(MAX_LINE_CHARS + 1);
    let toks = lx.lex_line(&long);
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, None);
    assert_eq!(toks[0].range, 0..long.len());
}

#[test]
fn multibyte_cap_counts_chars_not_bytes() {
    let mut lx = Lexer::for_lang(Lang::Rust);
    // 1500 cyrillic chars = 3000 bytes — under the char cap, still tokenized.
    let line = "\u{044f}".repeat(1500);
    let toks = lx.lex_line(&line);
    assert!(toks.len() >= 1);
    assert_eq!(toks.last().map(|t| t.range.end), Some(line.len()));
}

#[test]
fn detect_convenience() {
    assert!(Lexer::detect("main.rs").is_some());
    assert!(Lexer::detect("main.cobol").is_none());
    assert_eq!(
        Lexer::detect("ts").map(|l| l.lang()),
        Some(Lang::TypeScript)
    );
}
