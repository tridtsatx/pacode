//! Per-language tree-sitter queries and post-processing rules.
//!
//! Capture contract — every pattern binds:
//! - exactly one `@<kind>` capture (a [`crate::SymbolKind::name`]: `@fn`,
//!   `@class`, `@method`, …) over the node whose span becomes the symbol's
//!   line range;
//! - `@name` over the node(s) whose text is the display name. Multiple `@name`
//!   captures in one match merge into the contiguous text range covering them
//!   (`impl Trait for X` → `Trait for X`). Patterns may omit `@name`; the
//!   symbol then gets a per-language [`fallback_name`];
//! - optionally `@container`, whose text prefixes the name with the
//!   language's [`name_sep`] (`func (s *Server) M` → `Server.M`).
//!
//! When several patterns hit the same `(name, start_byte)` or the same
//! `(kind, span)`, the match from the pattern declared *earlier* in the source
//! wins — so specific patterns precede generic ones.
//!
//! Field constraints are ordered: `receiver:` must precede `name:` in a Go
//! `method_declaration` pattern because `func (r T) M` puts the receiver
//! first in the source.

use crate::extract::SymbolKind;
use crate::language::Language;

/// Symbol kinds that turn a nested `fn` into a `method`, per language:
/// a `fn` directly inside one of these parent kinds is reclassified.
pub fn method_parents(lang: Language) -> &'static [SymbolKind] {
    use SymbolKind as K;
    match lang {
        Language::Rust => &[K::Impl, K::Trait],
        Language::Python => &[K::Class],
        Language::JavaScript | Language::TypeScript | Language::Tsx => &[K::Class],
        Language::Go => &[],
        Language::C => &[],
        Language::Cpp => &[K::Class, K::Struct],
        Language::Java | Language::CSharp => &[K::Class, K::Struct, K::Interface, K::Enum],
        Language::Ruby => &[K::Class, K::Module],
        Language::Php => &[K::Class, K::Interface, K::Trait, K::Enum],
        Language::Swift => &[K::Class, K::Struct, K::Enum, K::Interface, K::Impl],
        Language::Kotlin => &[K::Class, K::Interface, K::Enum],
        Language::Scala => &[K::Class, K::Trait, K::Module, K::Enum, K::Impl],
        Language::Lua | Language::Bash => &[],
    }
}

/// Separator between a `@container` capture and the name (`Server.M` for Go,
/// `Foo::m` for C++).
pub fn name_sep(lang: Language) -> &'static str {
    match lang {
        Language::Rust | Language::Cpp | Language::Scala => "::",
        _ => ".",
    }
}

/// Display name for a match without `@name` (anonymous-token-only heads like
/// `init`/`deinit`/`constructor`). Falls back to the node kind stripped of the
/// usual `_declaration`/`_definition` suffix.
pub fn fallback_name(lang: Language, node_kind: &str) -> String {
    let fixed = match (lang, node_kind) {
        (Language::Swift, "init_declaration") => Some("init"),
        (Language::Swift, "deinit_declaration") => Some("deinit"),
        (Language::Kotlin, "secondary_constructor") => Some("constructor"),
        (Language::Kotlin, "primary_constructor") => Some("constructor"),
        (Language::CSharp, "indexer_declaration") => Some("this"),
        (Language::CSharp, "constructor_declaration")
        | (Language::CSharp, "destructor_declaration") => Some("ctor"),
        (Language::Java, "constructor_declaration")
        | (Language::Java, "compact_constructor_declaration") => Some("ctor"),
        _ => None,
    };
    if let Some(name) = fixed {
        return name.to_string();
    }
    node_kind
        .strip_suffix("_declaration")
        .or_else(|| node_kind.strip_suffix("_definition"))
        .unwrap_or(node_kind)
        .to_string()
}

pub fn query_src(lang: Language) -> &'static str {
    match lang {
        Language::Rust => RUST,
        Language::Python => PYTHON,
        Language::JavaScript => JAVASCRIPT,
        Language::TypeScript | Language::Tsx => TYPESCRIPT,
        Language::Go => GO,
        Language::C => C_LANG,
        Language::Cpp => CPP,
        Language::Java => JAVA,
        Language::CSharp => CSHARP,
        Language::Ruby => RUBY,
        Language::Php => PHP,
        Language::Swift => SWIFT,
        Language::Kotlin => KOTLIN,
        Language::Scala => SCALA,
        Language::Lua => LUA,
        Language::Bash => BASH,
    }
}

const RUST: &str = r#"
(impl_item trait: (_) @name type: (_) @name) @impl
(impl_item !trait type: (_) @name) @impl
(trait_item name: (type_identifier) @name) @trait
(function_item name: (identifier) @name) @fn
(function_signature_item name: (identifier) @name) @fn
(struct_item name: (type_identifier) @name) @struct
(enum_item name: (type_identifier) @name) @enum
(union_item name: (type_identifier) @name) @struct
(type_item name: (type_identifier) @name) @type
(associated_type name: (type_identifier) @name) @type
(mod_item name: (identifier) @name) @module
(macro_definition name: (identifier) @name) @macro
(const_item name: (identifier) @name) @const
(static_item name: (identifier) @name) @const
"#;

const PYTHON: &str = r#"
(decorated_definition (function_definition name: (identifier) @name)) @fn
(decorated_definition (class_definition name: (identifier) @name)) @class
(class_definition name: (identifier) @name) @class
(function_definition name: (identifier) @name) @fn
(module (expression_statement (assignment left: (identifier) @name) @const))
(class_definition body: (block (expression_statement (assignment left: (identifier) @name) @const)))
"#;

const JAVASCRIPT: &str = r#"
(function_declaration name: (identifier) @name) @fn
(generator_function_declaration name: (identifier) @name) @fn
(class_declaration name: (identifier) @name) @class
(method_definition name: [(property_identifier) (private_property_identifier)] @name) @method
(field_definition property: [(property_identifier) (private_property_identifier)] @name
                  value: [(arrow_function) (function_expression) (generator_function)]) @method
(export_statement
  declaration: [(lexical_declaration (variable_declarator name: (identifier) @name
                value: [(arrow_function) (function_expression) (generator_function)]) @fn)
              (variable_declaration (variable_declarator name: (identifier) @name
                value: [(arrow_function) (function_expression) (generator_function)]) @fn)])
(export_statement
  declaration: [(lexical_declaration (variable_declarator name: (identifier) @name) @export)
              (variable_declaration (variable_declarator name: (identifier) @name) @export)])
(program [(lexical_declaration (variable_declarator name: (identifier) @name
              value: [(arrow_function) (function_expression) (generator_function)]) @fn)
          (variable_declaration (variable_declarator name: (identifier) @name
              value: [(arrow_function) (function_expression) (generator_function)]) @fn)])
(program [(lexical_declaration (variable_declarator name: (identifier) @name) @const)
          (variable_declaration (variable_declarator name: (identifier) @name) @const)])
(export_statement (export_clause) @name) @export
(export_statement (namespace_export) @name) @export
(export_statement "default" (identifier) @name) @export
(export_statement value: (identifier) @name) @export
"#;

const TYPESCRIPT: &str = r#"
(function_declaration name: (identifier) @name) @fn
(generator_function_declaration name: (identifier) @name) @fn
(function_signature name: (identifier) @name) @fn
(class_declaration name: (type_identifier) @name) @class
(abstract_class_declaration name: (type_identifier) @name) @class
(interface_declaration name: (type_identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum
(type_alias_declaration name: (type_identifier) @name) @type
(internal_module name: (_) @name) @module
(module name: (_) @name) @module
(method_definition name: [(property_identifier) (private_property_identifier)] @name) @method
(method_signature name: (property_identifier) @name) @method
(abstract_method_signature name: (property_identifier) @name) @method
(public_field_definition name: [(property_identifier) (private_property_identifier)] @name
                         value: [(arrow_function) (function_expression) (generator_function)]) @method
(export_statement
  declaration: [(lexical_declaration (variable_declarator name: (identifier) @name
                value: [(arrow_function) (function_expression) (generator_function)]) @fn)
              (variable_declaration (variable_declarator name: (identifier) @name
                value: [(arrow_function) (function_expression) (generator_function)]) @fn)])
(export_statement
  declaration: [(lexical_declaration (variable_declarator name: (identifier) @name) @export)
              (variable_declaration (variable_declarator name: (identifier) @name) @export)])
(program [(lexical_declaration (variable_declarator name: (identifier) @name
              value: [(arrow_function) (function_expression) (generator_function)]) @fn)
          (variable_declaration (variable_declarator name: (identifier) @name
              value: [(arrow_function) (function_expression) (generator_function)]) @fn)])
(program [(lexical_declaration (variable_declarator name: (identifier) @name) @const)
          (variable_declaration (variable_declarator name: (identifier) @name) @const)])
(export_statement (export_clause) @name) @export
(export_statement (namespace_export) @name) @export
(export_statement "default" (identifier) @name) @export
(export_statement value: (identifier) @name) @export
"#;

const GO: &str = r#"
(function_declaration name: (identifier) @name) @fn
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [(pointer_type (type_identifier) @container) (type_identifier) @container]))
  name: (field_identifier) @name) @method
(type_spec name: (type_identifier) @name type: (struct_type)) @struct
(type_spec name: (type_identifier) @name type: (interface_type)) @interface
(type_spec name: (type_identifier) @name) @type
(type_alias name: (type_identifier) @name) @type
(source_file (const_declaration (const_spec name: (identifier) @name) @const))
(source_file (var_declaration (var_spec name: (identifier) @name) @const))
"#;

// C function declarators nest (`int *f()`, `char **f()`): the name-bearing
// `function_declarator` may sit under one or two `pointer_declarator`s or a
// `parenthesized_declarator`. Deeper shapes are rare enough to drop.
const C_LANG: &str = r#"
(function_definition
  declarator: [(function_declarator declarator: [(identifier) @name
                   (pointer_declarator declarator: (identifier) @name)
                   (parenthesized_declarator (identifier) @name)])
               (pointer_declarator declarator: (function_declarator
                   declarator: [(identifier) @name
                                (pointer_declarator declarator: (identifier) @name)
                                (parenthesized_declarator (identifier) @name)]))
               (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator
                   declarator: [(identifier) @name])))]) @fn
(declaration
  declarator: [(function_declarator
    declarator: [(identifier) @name
                 (pointer_declarator declarator: (identifier) @name)
                 (parenthesized_declarator (identifier) @name)])
               (pointer_declarator declarator: (function_declarator
    declarator: [(identifier) @name
                 (pointer_declarator declarator: (identifier) @name)
                 (parenthesized_declarator (identifier) @name)]))]) @fn
(struct_specifier name: (type_identifier) @name body: (_)) @struct
(union_specifier name: (type_identifier) @name body: (_)) @struct
(enum_specifier name: (type_identifier) @name body: (_)) @enum
(type_definition declarator: (type_identifier) @name) @type
(preproc_function_def name: (identifier) @name) @macro
(preproc_def name: (identifier) @name) @const
"#;

const CPP: &str = r#"
(function_definition
  declarator: [(function_declarator declarator: (qualified_identifier) @name)
               (pointer_declarator declarator: (function_declarator declarator: (qualified_identifier) @name))]) @method
(function_definition
  declarator: [(function_declarator
    declarator: [(identifier) @name
                 (field_identifier) @name
                 (qualified_identifier) @name
                 (operator_name) @name
                 (destructor_name) @name
                 (template_function name: (_) @name)
                 (pointer_declarator declarator: [(identifier) @name (field_identifier) @name])
                 (reference_declarator [(identifier) @name (field_identifier) @name])
                 (parenthesized_declarator [(identifier) @name (field_identifier) @name])])
               (pointer_declarator declarator: (function_declarator
    declarator: [(identifier) @name
                 (field_identifier) @name
                 (qualified_identifier) @name
                 (operator_name) @name
                 (destructor_name) @name
                 (template_function name: (_) @name)]))
               (reference_declarator (function_declarator
    declarator: [(identifier) @name (field_identifier) @name (qualified_identifier) @name]))]) @fn
(field_declaration
  declarator: (function_declarator
    declarator: [(field_identifier) @name
                 (identifier) @name
                 (operator_name) @name
                 (destructor_name) @name
                 (template_function name: (_) @name)])) @method
(declaration
  declarator: [(function_declarator
    declarator: [(identifier) @name
                 (field_identifier) @name
                 (qualified_identifier) @name
                 (operator_name) @name
                 (destructor_name) @name
                 (template_function name: (_) @name)])]) @fn
(struct_specifier name: (_) @name body: (_)) @struct
(union_specifier name: (_) @name body: (_)) @struct
(class_specifier name: (_) @name body: (_)) @class
(enum_specifier name: (_) @name body: (_)) @enum
(namespace_definition name: (_) @name) @module
(type_definition declarator: (type_identifier) @name) @type
(alias_declaration name: (type_identifier) @name) @type
(concept_definition name: (identifier) @name) @type
(preproc_function_def name: (identifier) @name) @macro
(preproc_def name: (identifier) @name) @const
"#;

const JAVA: &str = r#"
(class_declaration name: (identifier) @name) @class
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum
(record_declaration name: (identifier) @name) @class
(annotation_type_declaration name: (identifier) @name) @interface
(method_declaration name: (identifier) @name) @method
(constructor_declaration name: (identifier) @name) @method
(compact_constructor_declaration name: (identifier) @name) @method
(annotation_type_element_declaration name: (identifier) @name) @method
(constant_declaration declarator: (variable_declarator name: (identifier) @name)) @const
(module_declaration name: (_) @name) @module
"#;

const CSHARP: &str = r#"
(class_declaration name: (identifier) @name) @class
(interface_declaration name: (identifier) @name) @interface
(struct_declaration name: (identifier) @name) @struct
(record_declaration name: (identifier) @name) @class
(enum_declaration name: (identifier) @name) @enum
(namespace_declaration name: (_) @name) @module
(file_scoped_namespace_declaration name: (_) @name) @module
(method_declaration name: (identifier) @name) @method
(constructor_declaration name: (identifier) @name) @method
(destructor_declaration name: (identifier) @name) @method
(local_function_statement name: (identifier) @name) @fn
(delegate_declaration name: (identifier) @name) @type
(field_declaration (modifier "const")
  (variable_declaration (variable_declarator name: (identifier) @name)) @const)
(global_statement
  (local_declaration_statement (modifier "const")
    (variable_declaration (variable_declarator name: (identifier) @name)) @const))
(operator_declaration operator: (_) @name) @method
(conversion_operator_declaration type: (_) @name) @method
(operator_declaration) @method
(indexer_declaration) @method
"#;

const RUBY: &str = r#"
(class name: [(constant) @name (scope_resolution) @name]) @class
(singleton_class value: (_) @name) @class
(module name: [(constant) @name (scope_resolution) @name]) @module
(method name: (_) @name) @method
(singleton_method object: (_) @container name: (_) @name) @method
(alias name: (_) @name) @method
(assignment left: (constant) @name) @const
"#;

const PHP: &str = r#"
(class_declaration name: (name) @name) @class
(interface_declaration name: (name) @name) @interface
(trait_declaration name: (name) @name) @trait
(enum_declaration name: (name) @name) @enum
(function_definition name: (name) @name) @fn
(method_declaration name: (name) @name) @method
(namespace_definition name: (namespace_name) @name) @module
(const_declaration (const_element (name) @name) @const)
(enum_case name: (name) @name) @const
"#;

const SWIFT: &str = r#"
(class_declaration declaration_kind: "class" name: [(type_identifier) @name (user_type (type_identifier) @name)]) @class
(class_declaration declaration_kind: "actor" name: [(type_identifier) @name (user_type (type_identifier) @name)]) @class
(class_declaration declaration_kind: "struct" name: [(type_identifier) @name (user_type (type_identifier) @name)]) @struct
(class_declaration declaration_kind: "enum" name: [(type_identifier) @name (user_type (type_identifier) @name)]) @enum
(class_declaration declaration_kind: "extension" name: [(type_identifier) @name (user_type (type_identifier) @name)]) @impl
(protocol_declaration name: [(type_identifier) @name (user_type (type_identifier) @name)]) @interface
(function_declaration name: [(simple_identifier) @name (custom_operator) @name (user_type (type_identifier) @name)]) @fn
(function_declaration) @fn
(protocol_function_declaration name: [(simple_identifier) @name (user_type (type_identifier) @name)]) @method
(init_declaration) @method
(deinit_declaration) @method
(typealias_declaration name: (type_identifier) @name) @type
(associatedtype_declaration name: (type_identifier) @name) @type
(property_declaration name: (pattern bound_identifier: (simple_identifier) @name)) @const
(enum_entry name: (simple_identifier) @name) @const
"#;

const KOTLIN: &str = r#"
(class_declaration "interface" (type_identifier) @name) @interface
(class_declaration (type_identifier) @name (enum_class_body)) @enum
(class_declaration (type_identifier) @name) @class
(object_declaration (type_identifier) @name) @class
(companion_object (type_identifier) @name) @class
(function_declaration
  receiver: (receiver_type (user_type (type_identifier) @container))
  (simple_identifier) @name) @fn
(function_declaration (simple_identifier) @name) @fn
(source_file (property_declaration (variable_declaration (simple_identifier) @name) @const))
(class_body (property_declaration (variable_declaration (simple_identifier) @name) @const))
(type_alias (type_identifier) @name) @type
(secondary_constructor) @method
"#;

const SCALA: &str = r#"
(class_definition name: (identifier) @name) @class
(object_definition name: (identifier) @name) @module
(package_object name: (identifier) @name) @module
(trait_definition name: (identifier) @name) @trait
(enum_definition name: (identifier) @name) @enum
(extension_definition) @impl
(given_definition name: (identifier) @name) @impl
(function_definition name: (identifier) @name) @fn
(function_declaration name: (identifier) @name) @method
(type_definition name: (type_identifier) @name) @type
(package_clause name: (package_identifier) @name) @module
(template_body (val_definition pattern: (identifier) @name) @const)
(template_body (var_definition pattern: (identifier) @name) @const)
"#;

const LUA: &str = r#"
(function_declaration name: (method_index_expression) @name) @method
(function_declaration name: [(identifier) @name (dot_index_expression) @name]) @fn
(assignment_statement
  (variable_list name: [(identifier) @name (dot_index_expression) @name])
  (expression_list value: (function_definition))) @fn
(table_constructor (field name: (identifier) @name value: (function_definition)) @fn)
"#;

const BASH: &str = r#"
(function_definition name: (word) @name) @fn
(program (variable_assignment name: (variable_name) @name) @const)
"#;
