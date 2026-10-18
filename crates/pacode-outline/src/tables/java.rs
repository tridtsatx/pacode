//! Java node kinds (tree-sitter-java 0.23).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Java.
pub(crate) static JAVA: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "class", Body::Auto),
        Rule::new(
            "interface_declaration",
            Role::Container,
            "interface",
            Body::Auto,
        ),
        Rule::new("enum_declaration", Role::Container, "enum", Body::Auto),
        Rule::new("record_declaration", Role::Container, "record", Body::Auto),
        Rule::new(
            "annotation_type_declaration",
            Role::Container,
            "@interface",
            Body::Auto,
        ),
        Rule::new("method_declaration", Role::Callable, "method", Body::Auto),
        Rule::new(
            "constructor_declaration",
            Role::Callable,
            "constructor",
            Body::Auto,
        ),
        Rule::new(
            "compact_constructor_declaration",
            Role::Callable,
            "constructor",
            Body::Auto,
        ),
        Rule::new("static_initializer", Role::Callable, "static", Body::Auto),
        Rule::new("field_declaration", Role::Binding, "field", Body::Auto),
        Rule::new("constant_declaration", Role::Binding, "const", Body::Auto),
        Rule::new("import_declaration", Role::Import, "", Body::Auto),
        Rule::new("line_comment", Role::Comment, "", Body::Auto),
        Rule::new("block_comment", Role::Comment, "", Body::Auto),
        Rule::new("enum_body_declarations", Role::Wrapper, "", Body::Auto),
    ],
    body_kinds: &["block"],
    name_kinds: &["variable_declarator"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
