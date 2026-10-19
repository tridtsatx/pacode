//! `Language::from_path` / `from_name` mapping tests, including the
//! unsupported-extension path the `symbols` tool reports as `no grammar`.
use std::path::Path;

use pacode_symbols::Language;

fn lang(path: &str) -> Option<Language> {
    Language::from_path(Path::new(path))
}

#[test]
fn extension_mapping() {
    assert_eq!(lang("a/b.rs"), Some(Language::Rust));
    assert_eq!(lang("x.py"), Some(Language::Python));
    assert_eq!(lang("x.pyi"), Some(Language::Python));
    assert_eq!(lang("x.js"), Some(Language::JavaScript));
    assert_eq!(lang("x.jsx"), Some(Language::JavaScript));
    assert_eq!(lang("x.mjs"), Some(Language::JavaScript));
    assert_eq!(lang("x.ts"), Some(Language::TypeScript));
    assert_eq!(lang("x.tsx"), Some(Language::Tsx));
    assert_eq!(lang("x.go"), Some(Language::Go));
    assert_eq!(lang("x.c"), Some(Language::C));
    assert_eq!(lang("x.h"), Some(Language::C));
    assert_eq!(lang("x.cpp"), Some(Language::Cpp));
    assert_eq!(lang("x.hpp"), Some(Language::Cpp));
    assert_eq!(lang("x.java"), Some(Language::Java));
    assert_eq!(lang("x.cs"), Some(Language::CSharp));
    assert_eq!(lang("x.rb"), Some(Language::Ruby));
    assert_eq!(lang("x.php"), Some(Language::Php));
    assert_eq!(lang("x.swift"), Some(Language::Swift));
    assert_eq!(lang("x.kt"), Some(Language::Kotlin));
    assert_eq!(lang("x.kts"), Some(Language::Kotlin));
    assert_eq!(lang("x.scala"), Some(Language::Scala));
    assert_eq!(lang("x.lua"), Some(Language::Lua));
    assert_eq!(lang("x.sh"), Some(Language::Bash));
    assert_eq!(lang("x.bash"), Some(Language::Bash));
}

#[test]
fn special_file_names() {
    assert_eq!(lang(".bashrc"), Some(Language::Bash));
    assert_eq!(lang("/home/u/.bash_profile"), Some(Language::Bash));
    assert_eq!(lang(".zshrc"), Some(Language::Bash));
    // Known code-adjacent names without a grammar → `None` (reported as
    // `no grammar` by the tool, never silently dropped).
    assert_eq!(lang("Makefile"), None);
    assert_eq!(lang("Dockerfile"), None);
    assert_eq!(lang("CMakeLists.txt"), None);
}

#[test]
fn unsupported_extensions() {
    assert_eq!(lang("x.txt"), None);
    assert_eq!(lang("x.md"), None);
    assert_eq!(lang("x.yaml"), None);
    assert_eq!(lang("x.json"), None);
    assert_eq!(lang("x.min.js.map"), None);
    assert_eq!(lang("no_extension"), None);
    assert_eq!(lang("archive.tar.gz"), None);
}

#[test]
fn from_name_aliases() {
    assert_eq!(Language::from_name("rust"), Some(Language::Rust));
    assert_eq!(Language::from_name("rs"), Some(Language::Rust));
    assert_eq!(Language::from_name("py"), Some(Language::Python));
    assert_eq!(Language::from_name("ts"), Some(Language::TypeScript));
    assert_eq!(Language::from_name("c++"), Some(Language::Cpp));
    assert_eq!(Language::from_name("c#"), Some(Language::CSharp));
    assert_eq!(Language::from_name("sh"), Some(Language::Bash));
    assert_eq!(Language::from_name("Rust"), Some(Language::Rust));
    assert_eq!(Language::from_name("cobol"), None);
}

#[test]
fn every_language_roundtrips_name() {
    for lang in Language::ALL {
        assert_eq!(Language::from_name(lang.name()), Some(*lang));
    }
}
