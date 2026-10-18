//! Kotlin node kinds (tree-sitter-kotlin-ng 1.1).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Kotlin. No `body` fields in this grammar: bodies are found by kind.
pub(crate) static KOTLIN: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "class", Body::Auto),
        Rule::new("object_declaration", Role::Container, "object", Body::Auto),
        Rule::new(
            "companion_object",
            Role::Container,
            "companion object",
            Body::Auto,
        ),
        Rule::new("function_declaration", Role::Callable, "fun", Body::Auto),
        Rule::new(
            "secondary_constructor",
            Role::Callable,
            "constructor",
            Body::Auto,
        ),
        Rule::new("anonymous_initializer", Role::Callable, "init", Body::Auto),
        Rule::new("getter", Role::Callable, "get", Body::Auto),
        Rule::new("setter", Role::Callable, "set", Body::Auto),
        Rule::new("property_declaration", Role::Binding, "val", Body::Auto),
        Rule::new("type_alias", Role::Binding, "typealias", Body::Auto),
        Rule::new("import", Role::Import, "", Body::Auto),
        Rule::new("line_comment", Role::Comment, "", Body::Auto),
        Rule::new("block_comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &["class_body", "enum_class_body", "function_body", "block"],
    name_kinds: &["variable_declaration", "identifier"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
