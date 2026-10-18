//! Rust node kinds (tree-sitter-rust 0.24).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Rust.
pub(crate) static RUST: LangSpec = LangSpec {
    rules: &[
        Rule::new("impl_item", Role::Container, "impl", Body::Auto),
        Rule::new("trait_item", Role::Container, "trait", Body::Auto),
        Rule::new("mod_item", Role::Container, "mod", Body::Auto),
        Rule::new("struct_item", Role::Container, "struct", Body::Auto),
        Rule::new("enum_item", Role::Container, "enum", Body::Auto),
        Rule::new("union_item", Role::Container, "union", Body::Auto),
        Rule::new("foreign_mod_item", Role::Container, "extern", Body::Auto),
        Rule::new("function_item", Role::Callable, "fn", Body::Auto),
        Rule::new("function_signature_item", Role::Callable, "fn", Body::Auto),
        Rule::new("macro_definition", Role::Callable, "macro", Body::SelfNode),
        Rule::new("const_item", Role::Binding, "const", Body::Auto),
        Rule::new("static_item", Role::Binding, "static", Body::Auto),
        Rule::new("type_item", Role::Binding, "type", Body::Auto),
        Rule::new("let_declaration", Role::Binding, "let", Body::Auto),
        Rule::new("use_declaration", Role::Import, "", Body::Auto),
        Rule::new("extern_crate_declaration", Role::Import, "", Body::Auto),
        Rule::new("line_comment", Role::Comment, "", Body::Auto),
        Rule::new("block_comment", Role::Comment, "", Body::Auto),
        Rule::new("attribute_item", Role::Attribute, "", Body::Auto),
        Rule::new("inner_attribute_item", Role::Attribute, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &[],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
