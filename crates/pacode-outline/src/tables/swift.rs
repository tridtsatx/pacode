//! Swift node kinds (tree-sitter-swift 0.7).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Swift. `class_declaration` also covers struct/enum/extension/actor; the label is the keyword.
pub(crate) static SWIFT: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "", Body::Auto),
        Rule::new(
            "protocol_declaration",
            Role::Container,
            "protocol",
            Body::Auto,
        ),
        Rule::new("function_declaration", Role::Callable, "func", Body::Auto),
        Rule::new("init_declaration", Role::Callable, "init", Body::Auto),
        Rule::new("deinit_declaration", Role::Callable, "deinit", Body::Auto),
        Rule::new(
            "protocol_function_declaration",
            Role::Callable,
            "func",
            Body::Auto,
        ),
        Rule::new(
            "subscript_declaration",
            Role::Callable,
            "subscript",
            Body::Auto,
        ),
        Rule::new("property_declaration", Role::Binding, "var", Body::Auto),
        Rule::new(
            "typealias_declaration",
            Role::Binding,
            "typealias",
            Body::Auto,
        ),
        Rule::new("import_declaration", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("multiline_comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &[],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
