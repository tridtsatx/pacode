use pretty_assertions::assert_eq;

use crate::{Syntax, mask_code, rust_test_lines};

fn sp(n: usize) -> String {
    " ".repeat(n)
}

fn rs(s: &str) -> String {
    mask_code(s, Syntax::Rust)
}

#[test]
fn mask_preserves_length_and_newlines() {
    let src = "let s = \"héllo\"; // ünï\n/* a\nb */ let c = 'é';\n";
    for syn in [Syntax::Rust, Syntax::CLike, Syntax::Hash] {
        let out = mask_code(src, syn);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.matches('\n').count(), src.matches('\n').count());
    }
}

#[test]
fn rust_strings_comments_chars() {
    assert_eq!(
        rs("a(\"x.unwrap()\"); // y.unwrap()"),
        format!("a(\"{}\"); {}", sp(10), sp(13))
    );
    assert_eq!(rs("let c = 'x';"), "let c = ' ';");
    assert_eq!(rs("let c = '\\n';"), "let c = '  ';");
    assert_eq!(rs("fn f<'a>(x: &'a str) {}"), "fn f<'a>(x: &'a str) {}");
    assert_eq!(rs("/* a /* b */ c */ x"), format!("{}x", sp(18)));
    assert_eq!(rs("let s = r#\"a\"b\"#; z"), "let s = r#\"   \"#; z");
    assert_eq!(rs("let s = \"a\\\"b\"; z"), "let s = \"    \"; z");
}

#[test]
fn clike_template_and_quotes() {
    let m = mask_code("x(`a\nb`); y('z') // c", Syntax::CLike);
    assert_eq!(m, format!("x(` \n `); y(' ') {}", sp(4)));
    // unterminated quote ends at the newline
    let m = mask_code("don't\nx", Syntax::CLike);
    assert_eq!(m, "don' \nx");
}

#[test]
fn hash_syntax() {
    assert_eq!(
        mask_code("a = \"x\" # c\nb", Syntax::Hash),
        format!("a = \" \" {}\nb", sp(3))
    );
    assert_eq!(
        mask_code("\"\"\"abc\ndef\"\"\" z", Syntax::Hash),
        "\"\"\"   \n   \"\"\" z"
    );
}

#[test]
fn test_regions() {
    let src = "fn a() {}\n#[cfg(test)]\nmod t {\n    fn x() {\n    }\n}\nfn b() {}\n";
    let flags = rust_test_lines(&rs(src));
    assert_eq!(flags, vec![false, true, true, true, true, true, false]);

    let one_line = "#[test] fn t() { x(); }\nfn b() {}\n";
    assert_eq!(rust_test_lines(&rs(one_line)), vec![true, false]);

    let braces_in_string = "#[cfg(test)]\nmod t {\n  const S: &str = \"}\";\n}\nfn z() {}\n";
    assert_eq!(
        rust_test_lines(&rs(braces_in_string)),
        vec![true, true, true, true, false]
    );
}
