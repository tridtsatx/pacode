use std::ops::Range;

use pretty_assertions::assert_eq;

use super::*;

fn sym(kind: &'static str, name: &str, lines: Range<usize>, kids: Vec<Symbol>) -> Symbol {
    Symbol {
        kind,
        name: name.to_string(),
        lines,
        children: kids,
    }
}

/// `struct Foo`, `impl Foo { fn new, fn short }`, `mod tests { fn new }`, an anonymous
/// function holding `fn inner`, and C++-style `A::g`.
fn sample() -> Outline {
    Outline {
        total_lines: 100,
        symbols: vec![
            sym("struct", "Foo", 0..5, vec![]),
            sym(
                "impl",
                "Foo<T>",
                6..20,
                vec![
                    sym("fn", "new", 7..12, vec![]),
                    sym("fn", "short", 13..19, vec![]),
                ],
            ),
            sym(
                "mod",
                "tests",
                21..40,
                vec![sym("fn", "new", 22..30, vec![])],
            ),
            sym("fn", "", 41..50, vec![sym("fn", "inner", 42..49, vec![])]),
            sym("fn", "A::g", 51..60, vec![]),
        ],
        ..Outline::default()
    }
}

fn found(query: &str) -> Vec<(String, Range<usize>)> {
    find_symbol(&sample(), query)
        .into_iter()
        .map(|h| (h.path, h.lines))
        .collect()
}

#[test]
fn bare_name_matches_at_any_depth() {
    assert_eq!(
        found("new"),
        [
            ("Foo::new".to_string(), 7..12),
            ("tests::new".to_string(), 22..30)
        ]
    );
}

#[test]
fn qualified_query_narrows_by_ancestors() {
    assert_eq!(found("Foo::new"), [("Foo::new".to_string(), 7..12)]);
    assert_eq!(found("Foo.short"), [("Foo::short".to_string(), 13..19)]);
}

#[test]
fn generic_arguments_are_ignored() {
    let hits = find_symbol(&sample(), "Foo");
    let kinds: Vec<&str> = hits.iter().map(|h| h.kind).collect();
    assert_eq!(kinds, ["struct", "impl"]);
    assert_eq!(found("Foo<U>::short"), [("Foo::short".to_string(), 13..19)]);
}

#[test]
fn qualified_names_match_by_segment() {
    assert_eq!(found("g"), [("A::g".to_string(), 51..60)]);
    assert_eq!(found("A::g"), [("A::g".to_string(), 51..60)]);
}

#[test]
fn anonymous_symbols_are_transparent() {
    assert_eq!(found("inner"), [("inner".to_string(), 42..49)]);
}

#[test]
fn case_insensitive_only_without_exact_match() {
    assert_eq!(found("FOO::NEW"), [("Foo::new".to_string(), 7..12)]);
}

#[test]
fn unknown_or_empty_query_finds_nothing() {
    assert!(found("missing").is_empty());
    assert!(found("  ").is_empty());
    assert!(found("::").is_empty());
}

#[test]
fn symbol_paths_lists_named_symbols_in_order() {
    assert_eq!(
        symbol_paths(&sample()),
        [
            "Foo",
            "Foo<T>",
            "Foo<T>::new",
            "Foo<T>::short",
            "tests",
            "tests::new",
            "inner",
            "A::g"
        ]
    );
}

#[cfg(feature = "python")]
#[test]
fn finds_python_method_in_real_outline() {
    let source = "class Repo:\n    def name(self):\n        return 1\n\n\ndef main():\n    pass\n";
    let o = outline(source, Lang::Python, &OutlineOpts::default()).expect("outline");
    let hits = find_symbol(&o, "Repo.name");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].lines, 1..3);
    assert_eq!(hits[0].kind, "def");
}
