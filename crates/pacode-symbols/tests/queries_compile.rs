//! Every language's query source must compile against its grammar — this is
//! the contract between `queries.rs` and the grammar crates' node names.

use pacode_symbols::Language;

#[test]
fn all_queries_compile() {
    let mut failures = Vec::new();
    for &lang in Language::ALL {
        let ts_lang = lang.ts_language();
        if let Err(e) = tree_sitter::Query::new(&ts_lang, pacode_symbols::queries::query_src(lang))
        {
            failures.push(format!("{lang}: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "query compile failures:\n{}",
        failures.join("\n")
    );
}
