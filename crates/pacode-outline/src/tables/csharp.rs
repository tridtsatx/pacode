//! C# node kinds (tree-sitter-c-sharp 0.23).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// C#.
pub(crate) static CSHARP: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "class", Body::Auto),
        Rule::new("struct_declaration", Role::Container, "struct", Body::Auto),
        Rule::new(
            "interface_declaration",
            Role::Container,
            "interface",
            Body::Auto,
        ),
        Rule::new("enum_declaration", Role::Container, "enum", Body::Auto),
        Rule::new("record_declaration", Role::Container, "record", Body::Auto),
        Rule::new(
            "namespace_declaration",
            Role::Container,
            "namespace",
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
            "destructor_declaration",
            Role::Callable,
            "destructor",
            Body::Auto,
        ),
        Rule::new(
            "operator_declaration",
            Role::Callable,
            "operator",
            Body::Auto,
        ),
        Rule::new(
            "conversion_operator_declaration",
            Role::Callable,
            "operator",
            Body::Auto,
        ),
        Rule::new("local_function_statement", Role::Callable, "fn", Body::Auto),
        Rule::new("field_declaration", Role::Binding, "field", Body::Auto),
        Rule::new(
            "property_declaration",
            Role::Binding,
            "property",
            Body::Auto,
        ),
        Rule::new(
            "event_field_declaration",
            Role::Binding,
            "event",
            Body::Auto,
        ),
        Rule::new("event_declaration", Role::Binding, "event", Body::Auto),
        Rule::new(
            "delegate_declaration",
            Role::Binding,
            "delegate",
            Body::Auto,
        ),
        Rule::new(
            "file_scoped_namespace_declaration",
            Role::Binding,
            "namespace",
            Body::Auto,
        ),
        Rule::new("using_directive", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["variable_declaration", "variable_declarator"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
