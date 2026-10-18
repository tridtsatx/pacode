//! C and C++ node kinds (tree-sitter-c 0.24, tree-sitter-cpp 0.23).
//! Data only: the walk in `crate::walk` is language-agnostic.

use crate::spec::{Body, LangSpec, Role, Rule};

/// C. Preprocessor conditionals are containers whose members are their own children.
pub(crate) static C: LangSpec = LangSpec {
    rules: &[
        Rule::new("function_definition", Role::Callable, "fn", Body::Auto),
        Rule::new("struct_specifier", Role::Container, "struct", Body::Auto),
        Rule::new("union_specifier", Role::Container, "union", Body::Auto),
        Rule::new("enum_specifier", Role::Container, "enum", Body::Auto),
        Rule::new("type_definition", Role::Container, "typedef", Body::Auto),
        Rule::new("preproc_ifdef", Role::Container, "#ifdef", Body::SelfNode),
        Rule::new("preproc_if", Role::Container, "#if", Body::SelfNode),
        Rule::new("preproc_else", Role::Container, "#else", Body::SelfNode),
        Rule::new("preproc_elif", Role::Container, "#elif", Body::SelfNode),
        Rule::new(
            "preproc_elifdef",
            Role::Container,
            "#elifdef",
            Body::SelfNode,
        ),
        Rule::new("declaration", Role::Binding, "decl", Body::Auto),
        Rule::new("field_declaration", Role::Binding, "field", Body::Auto),
        Rule::new("preproc_def", Role::Binding, "#define", Body::Auto),
        Rule::new("preproc_function_def", Role::Binding, "#define", Body::Auto),
        Rule::new("preproc_include", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
    ],
    body_kinds: &["field_declaration_list", "enumerator_list"],
    name_kinds: &[],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};

/// C++ (the C kinds plus classes, namespaces and templates).
pub(crate) static CPP: LangSpec = LangSpec {
    rules: &[
        Rule::new("function_definition", Role::Callable, "fn", Body::Auto),
        Rule::new("struct_specifier", Role::Container, "struct", Body::Auto),
        Rule::new("union_specifier", Role::Container, "union", Body::Auto),
        Rule::new("enum_specifier", Role::Container, "enum", Body::Auto),
        Rule::new("type_definition", Role::Container, "typedef", Body::Auto),
        Rule::new("preproc_ifdef", Role::Container, "#ifdef", Body::SelfNode),
        Rule::new("preproc_if", Role::Container, "#if", Body::SelfNode),
        Rule::new("preproc_else", Role::Container, "#else", Body::SelfNode),
        Rule::new("preproc_elif", Role::Container, "#elif", Body::SelfNode),
        Rule::new(
            "preproc_elifdef",
            Role::Container,
            "#elifdef",
            Body::SelfNode,
        ),
        Rule::new("declaration", Role::Binding, "decl", Body::Auto),
        Rule::new("field_declaration", Role::Binding, "field", Body::Auto),
        Rule::new("preproc_def", Role::Binding, "#define", Body::Auto),
        Rule::new("preproc_function_def", Role::Binding, "#define", Body::Auto),
        Rule::new("preproc_include", Role::Import, "", Body::Auto),
        Rule::new("comment", Role::Comment, "", Body::Auto),
        Rule::new("class_specifier", Role::Container, "class", Body::Auto),
        Rule::new(
            "namespace_definition",
            Role::Container,
            "namespace",
            Body::Auto,
        ),
        Rule::new(
            "linkage_specification",
            Role::Container,
            "extern",
            Body::Auto,
        ),
        Rule::new("alias_declaration", Role::Binding, "using", Body::Auto),
        Rule::new("using_declaration", Role::Import, "", Body::Auto),
        Rule::new("template_declaration", Role::Wrapper, "", Body::Auto),
    ],
    body_kinds: &["field_declaration_list", "enumerator_list"],
    name_kinds: &[],
    max_depth: 4,
    data: false,
    lead: false,
    keep_first_row: true,
};
