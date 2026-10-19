use super::*;

#[test]
fn detects_by_extension() {
    assert_eq!(detect_lang("src/main.rs"), Some(Lang::Rust));
    assert_eq!(detect_lang("b/src/main.rs"), Some(Lang::Rust));
    assert_eq!(detect_lang("a/lib/mod.rs"), Some(Lang::Rust));
    assert_eq!(detect_lang("test.py"), Some(Lang::Python));
    assert_eq!(detect_lang("app.tsx"), Some(Lang::TypeScript));
    assert_eq!(detect_lang("app.js"), Some(Lang::TypeScript));
    assert_eq!(detect_lang("data.json"), Some(Lang::Json));
    assert_eq!(detect_lang("Cargo.toml"), Some(Lang::Toml));
    assert_eq!(detect_lang("ci.yml"), Some(Lang::Yaml));
    assert_eq!(detect_lang("run.sh"), Some(Lang::Bash));
    assert_eq!(detect_lang("README.md"), Some(Lang::Markdown));
}

#[test]
fn detects_by_info_string() {
    assert_eq!(detect_lang("rust"), Some(Lang::Rust));
    assert_eq!(detect_lang("python"), Some(Lang::Python));
    assert_eq!(detect_lang("python3"), Some(Lang::Python));
    assert_eq!(detect_lang("ts"), Some(Lang::TypeScript));
    assert_eq!(detect_lang("javascript"), Some(Lang::TypeScript));
    assert_eq!(detect_lang("json"), Some(Lang::Json));
    assert_eq!(detect_lang("toml"), Some(Lang::Toml));
    assert_eq!(detect_lang("yaml"), Some(Lang::Yaml));
    assert_eq!(detect_lang("bash"), Some(Lang::Bash));
    assert_eq!(detect_lang("sh"), Some(Lang::Bash));
    assert_eq!(detect_lang("markdown"), Some(Lang::Markdown));
    // Info strings carry flags after the language.
    assert_eq!(detect_lang("rust,no_run"), Some(Lang::Rust));
    assert_eq!(detect_lang("python {linenos}"), Some(Lang::Python));
}

#[test]
fn detects_special_file_names() {
    assert_eq!(detect_lang(".bashrc"), Some(Lang::Bash));
    assert_eq!(detect_lang("/home/u/.zshrc"), Some(Lang::Bash));
    assert_eq!(detect_lang("Cargo.lock"), Some(Lang::Toml));
}

#[test]
fn unknown_is_none_not_an_error() {
    assert_eq!(detect_lang(""), None);
    assert_eq!(detect_lang("   "), None);
    assert_eq!(detect_lang("main.c"), None);
    assert_eq!(detect_lang("haskell"), None);
    assert_eq!(detect_lang("/dev/null"), None);
    assert_eq!(detect_lang("Makefile"), None);
    assert_eq!(detect_lang("x."), None);
}
