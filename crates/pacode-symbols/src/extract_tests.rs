use super::{FileSymbols, Language, extract, with_query};

#[test]
fn every_language_compiles_its_query_once() {
    for &lang in Language::ALL {
        let first = with_query(lang, |query| std::ptr::from_ref(query) as usize).unwrap();
        let again = with_query(lang, |query| std::ptr::from_ref(query) as usize).unwrap();
        assert_eq!(first, again, "{lang}: the compiled query must be reused");
    }
}

#[test]
fn a_cached_query_gives_the_same_outline() {
    let source = "struct A;\nimpl A {\n    fn f(&self) {}\n}\nfn g() {}\n";
    let names = |symbols: &FileSymbols| {
        symbols
            .symbols
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
    };
    let first = extract(source, Language::Rust, Default::default()).unwrap();
    let again = extract(source, Language::Rust, Default::default()).unwrap();
    assert!(
        names(&first).contains(&"f".to_string()),
        "{:?}",
        names(&first)
    );
    assert_eq!(names(&first), names(&again));
}
