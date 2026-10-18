//! Python node kinds (tree-sitter-python 0.25).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Python.
pub(crate) static PYTHON: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_definition", Role::Container, "class", Body::Auto),
        Rule::new("function_definition", Role::Callable, "def", Body::Auto),
        Rule::new("expression_statement", Role::Binding, "var", Body::Auto),
        Rule::new("type_alias_statement", Role::Binding, "type", Body::Auto),
        Rule::new("import_statement", Role::Import, "", Body::Auto),
        Rule::new("import_from_statement", Role::Import, "", Body::Auto),
        Rule::new("future_import_statement", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("decorator", Role::Attribute, "", Body::Auto),
        Rule::new("decorated_definition", Role::Wrapper, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["assignment"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
