//! Go node kinds (tree-sitter-go 0.25).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Go. `type_spec` has no `body` field: the struct field list / interface type is found by kind.
pub(crate) static GO: LangSpec = LangSpec {
    rules: &[
        Rule::new("type_spec", Role::Container, "type", Body::Auto),
        Rule::new("function_declaration", Role::Callable, "func", Body::Auto),
        Rule::new("method_declaration", Role::Callable, "func", Body::Auto),
        Rule::new("method_elem", Role::Callable, "func", Body::Auto),
        Rule::new("var_declaration", Role::Binding, "var", Body::Auto),
        Rule::new("const_declaration", Role::Binding, "const", Body::Auto),
        Rule::new("type_alias", Role::Binding, "type", Body::Auto),
        Rule::new("import_declaration", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("type_declaration", Role::Wrapper, "", Body::Auto),
    ],
    body_kinds: &["field_declaration_list", "interface_type"],
    name_kinds: &["var_spec", "const_spec", "type_spec"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
