//! Ruby node kinds (tree-sitter-ruby 0.23).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// Ruby. `require` is an ordinary call, so Ruby has no import kinds.
pub(crate) static RUBY: LangSpec = LangSpec {
    rules: &[
        Rule::new("class", Role::Container, "class", Body::Auto),
        Rule::new("module", Role::Container, "module", Body::Auto),
        Rule::new("singleton_class", Role::Container, "class", Body::Auto),
        Rule::new("method", Role::Callable, "def", Body::Auto),
        Rule::new("singleton_method", Role::Callable, "def", Body::Auto),
        Rule::new("assignment", Role::Binding, "var", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &["body_statement"],
    name_kinds: &[],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
