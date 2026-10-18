//! Markdown node kinds (tree-sitter-md 0.5, block grammar): headings and the first lines of each section.
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Markdown.
pub(crate) static MARKDOWN: LangSpec = LangSpec {
    rules: &[Rule::new(
        "section",
        Role::Container,
        "section",
        Body::SelfNode,
    )],
    body_kinds: &[],
    name_kinds: &["atx_heading", "setext_heading"],
    max_depth: 6,
    data: false,
    lead: true,
    keep_first_row: false,
};
