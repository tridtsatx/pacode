//! PHP node kinds (tree-sitter-php 0.25, the PHP-with-HTML grammar).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// PHP.
pub(crate) static PHP: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "class", Body::Auto),
        Rule::new(
            "interface_declaration",
            Role::Container,
            "interface",
            Body::Auto,
        ),
        Rule::new("trait_declaration", Role::Container, "trait", Body::Auto),
        Rule::new("enum_declaration", Role::Container, "enum", Body::Auto),
        Rule::new(
            "namespace_definition",
            Role::Container,
            "namespace",
            Body::Auto,
        ),
        Rule::new(
            "function_definition",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new("method_declaration", Role::Callable, "function", Body::Auto),
        Rule::new("const_declaration", Role::Binding, "const", Body::Auto),
        Rule::new(
            "property_declaration",
            Role::Binding,
            "property",
            Body::Auto,
        ),
        Rule::new("namespace_use_declaration", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("attribute_list", Role::Attribute, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["const_element", "property_element", "name"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
