//! Bash node kinds (tree-sitter-bash 0.25).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Bash.
pub(crate) static BASH: LangSpec = LangSpec {
    rules: &[
        Rule::new(
            "function_definition",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new("variable_assignment", Role::Binding, "var", Body::Auto),
        Rule::new("declaration_command", Role::Binding, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["variable_assignment"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
