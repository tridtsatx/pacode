use std::path::Path;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn detects_languages_by_extension_and_name() {
    let cases = [
        ("src/main.rs", Some(Lang::Rust)),
        ("a/b.TS", Some(Lang::TypeScript)),
        ("types.d.ts", Some(Lang::TypeScript)),
        ("App.tsx", Some(Lang::Tsx)),
        ("index.mjs", Some(Lang::JavaScript)),
        ("view.jsx", Some(Lang::JavaScript)),
        ("tool.py", Some(Lang::Python)),
        ("main.go", Some(Lang::Go)),
        ("Main.java", Some(Lang::Java)),
        ("lib.h", Some(Lang::C)),
        ("lib.hpp", Some(Lang::Cpp)),
        ("Program.cs", Some(Lang::CSharp)),
        ("build.gradle.kts", Some(Lang::Kotlin)),
        ("Gemfile", Some(Lang::Ruby)),
        ("index.php", Some(Lang::Php)),
        ("App.swift", Some(Lang::Swift)),
        ("run.sh", Some(Lang::Bash)),
        (".bashrc", Some(Lang::Bash)),
        ("package.json", Some(Lang::Json)),
        ("Cargo.lock", Some(Lang::Toml)),
        ("ci.yml", Some(Lang::Yaml)),
        ("README.md", Some(Lang::Markdown)),
        ("notes.txt", None),
        ("Makefile", None),
        ("no_extension", None),
    ];
    for (path, want) in cases {
        assert_eq!(Lang::from_path(Path::new(path)), want, "{path}");
    }
}

#[test]
fn every_language_has_a_table_and_a_name() {
    for lang in Lang::ALL {
        assert!(!lang.name().is_empty());
        let spec = lang.spec();
        assert!(!spec.rules.is_empty(), "{lang:?}");
        assert!(spec.max_depth > 0, "{lang:?}");
    }
}

#[test]
fn table_kinds_are_unique_per_language() {
    for lang in Lang::ALL {
        let rules = lang.spec().rules;
        for (i, rule) in rules.iter().enumerate() {
            let dup = rules.iter().skip(i + 1).any(|r| r.kind == rule.kind);
            assert!(!dup, "{lang:?}: {} listed twice", rule.kind);
        }
    }
}

/// With the default features every grammar loads into this tree-sitter (ABI check).
#[cfg(feature = "all-languages")]
#[test]
fn all_grammars_load() {
    for lang in Lang::ALL {
        let language = lang.grammar().expect("compiled in");
        let mut parser = tree_sitter::Parser::new();
        assert!(parser.set_language(&language).is_ok(), "{lang:?}");
    }
}

/// Every kind in a table is a named node kind of that grammar (catches typos and
/// renames when a grammar crate is upgraded).
#[cfg(feature = "all-languages")]
#[test]
fn table_kinds_exist_in_their_grammar() {
    for lang in Lang::ALL {
        let language = lang.grammar().expect("compiled in");
        let spec = lang.spec();
        let kinds = spec
            .rules
            .iter()
            .map(|r| r.kind)
            .chain(spec.body_kinds.iter().copied())
            .chain(spec.name_kinds.iter().copied());
        for kind in kinds {
            assert!(
                language.id_for_node_kind(kind, true) != 0,
                "{lang:?}: no node kind {kind}"
            );
        }
    }
}
