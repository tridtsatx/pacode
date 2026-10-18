//! JavaScript / TypeScript / TSX node kinds (tree-sitter-javascript 0.25, tree-sitter-typescript 0.23).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// JavaScript (JSX included).
pub(crate) static JAVASCRIPT: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "class", Body::Auto),
        Rule::new("class", Role::Container, "class", Body::Auto),
        Rule::new(
            "function_declaration",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new(
            "generator_function_declaration",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new(
            "function_expression",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new("arrow_function", Role::Callable, "fn", Body::Auto),
        Rule::new("method_definition", Role::Callable, "method", Body::Auto),
        Rule::new("lexical_declaration", Role::Binding, "", Body::Auto),
        Rule::new("variable_declaration", Role::Binding, "var", Body::Auto),
        Rule::new("import_statement", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("decorator", Role::Attribute, "", Body::Auto),
        Rule::new("export_statement", Role::Wrapper, "", Body::Auto),
        Rule::new("field_definition", Role::Binding, "field", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["variable_declarator"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};

/// TypeScript and TSX (the two grammars share node kinds).
pub(crate) static TYPESCRIPT: LangSpec = LangSpec {
    rules: &[
        Rule::new("class_declaration", Role::Container, "class", Body::Auto),
        Rule::new("class", Role::Container, "class", Body::Auto),
        Rule::new(
            "function_declaration",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new(
            "generator_function_declaration",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new(
            "function_expression",
            Role::Callable,
            "function",
            Body::Auto,
        ),
        Rule::new("arrow_function", Role::Callable, "fn", Body::Auto),
        Rule::new("method_definition", Role::Callable, "method", Body::Auto),
        Rule::new("lexical_declaration", Role::Binding, "", Body::Auto),
        Rule::new("variable_declaration", Role::Binding, "var", Body::Auto),
        Rule::new("import_statement", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("decorator", Role::Attribute, "", Body::Auto),
        Rule::new("export_statement", Role::Wrapper, "", Body::Auto),
        Rule::new(
            "abstract_class_declaration",
            Role::Container,
            "class",
            Body::Auto,
        ),
        Rule::new(
            "interface_declaration",
            Role::Container,
            "interface",
            Body::Auto,
        ),
        Rule::new("enum_declaration", Role::Container, "enum", Body::Auto),
        Rule::new("internal_module", Role::Container, "namespace", Body::Auto),
        Rule::new("module", Role::Container, "module", Body::Auto),
        Rule::new("function_signature", Role::Callable, "function", Body::Auto),
        Rule::new("method_signature", Role::Callable, "method", Body::Auto),
        Rule::new(
            "abstract_method_signature",
            Role::Callable,
            "method",
            Body::Auto,
        ),
        Rule::new("type_alias_declaration", Role::Binding, "type", Body::Auto),
        Rule::new(
            "public_field_definition",
            Role::Binding,
            "field",
            Body::Auto,
        ),
        Rule::new("ambient_declaration", Role::Wrapper, "", Body::Auto),
    ],
    body_kinds: &[],
    name_kinds: &["variable_declarator"],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
