//! JSON / TOML / YAML node kinds (tree-sitter-json 0.24, tree-sitter-toml-ng 0.7, tree-sitter-yaml 0.7): keys down to `max_depth`, deeper values collapsed to their first and last line.
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// JSON. The root object is a level of its own, so depth 3 shows three levels of keys (like YAML at 2).
pub(crate) static JSON: LangSpec = LangSpec {
    rules: &[
        Rule::new("pair", Role::Container, "key", Body::Field("value")),
        Rule::new("object", Role::Container, "", Body::SelfNode),
        Rule::new("array", Role::Container, "", Body::SelfNode),
        Rule::new("comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &[],
    max_depth: 3,
    data: true,
    lead: false,
    keep_first_row: true,
};

/// TOML: each table shows its header and first lines.
pub(crate) static TOML: LangSpec = LangSpec {
    rules: &[
        Rule::new("table", Role::Callable, "table", Body::SelfNode),
        Rule::new(
            "table_array_element",
            Role::Callable,
            "table",
            Body::SelfNode,
        ),
        Rule::new("pair", Role::Binding, "key", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["bare_key", "dotted_key", "quoted_key"],
    max_depth: 2,
    data: true,
    lead: false,
    keep_first_row: true,
};

/// YAML: mappings and sequences are transparent, pairs nest.
pub(crate) static YAML: LangSpec = LangSpec {
    rules: &[
        Rule::new(
            "block_mapping_pair",
            Role::Container,
            "key",
            Body::Field("value"),
        ),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("document", Role::Wrapper, "", Body::Auto),
        Rule::new("block_node", Role::Wrapper, "", Body::Auto),
        Rule::new("block_mapping", Role::Wrapper, "", Body::Auto),
        Rule::new("block_sequence", Role::Wrapper, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &[],
    max_depth: 2,
    data: true,
    lead: false,
    keep_first_row: true,
};
