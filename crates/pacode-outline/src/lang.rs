//! Supported languages: detection by path, grammar (feature-gated) and node-kind table.

use std::path::Path;

use tree_sitter::Language;

use crate::spec::LangSpec;
use crate::tables;

/// A language the outliner knows. Every variant exists whatever the Cargo features;
/// [`Lang::is_available`] says whether its grammar was compiled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Kotlin,
    Ruby,
    Php,
    Swift,
    Bash,
    Json,
    Toml,
    Yaml,
    Markdown,
}

impl Lang {
    pub const ALL: [Lang; 19] = [
        Lang::Rust,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::Python,
        Lang::Go,
        Lang::Java,
        Lang::C,
        Lang::Cpp,
        Lang::CSharp,
        Lang::Kotlin,
        Lang::Ruby,
        Lang::Php,
        Lang::Swift,
        Lang::Bash,
        Lang::Json,
        Lang::Toml,
        Lang::Yaml,
        Lang::Markdown,
    ];

    /// Detect by file name, then by extension (case-insensitive). `None` = not a
    /// language the outliner knows (whether or not its grammar is compiled in).
    pub fn from_path(path: &Path) -> Option<Lang> {
        let file = path.file_name()?.to_str()?;
        match file {
            "Gemfile" | "Rakefile" | "Guardfile" | "Podfile" | "Vagrantfile" => {
                return Some(Lang::Ruby);
            }
            ".bashrc" | ".bash_profile" | ".bash_logout" | ".profile" | "PKGBUILD" => {
                return Some(Lang::Bash);
            }
            "Cargo.lock" | "poetry.lock" | "uv.lock" | "Pipfile" => return Some(Lang::Toml),
            _ => {}
        }
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let lang = match ext.as_str() {
            "rs" => Lang::Rust,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "py" | "pyi" | "pyw" => Lang::Python,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "c" | "h" => Lang::C,
            "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "inl" => {
                Lang::Cpp
            }
            "cs" => Lang::CSharp,
            "kt" | "kts" => Lang::Kotlin,
            "rb" | "rake" | "gemspec" | "ru" => Lang::Ruby,
            "php" => Lang::Php,
            "swift" => Lang::Swift,
            "sh" | "bash" => Lang::Bash,
            "json" | "jsonc" => Lang::Json,
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "md" | "markdown" => Lang::Markdown,
            _ => return None,
        };
        Some(lang)
    }

    /// Display name (`"Rust"`, `"C#"`).
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::TypeScript => "TypeScript",
            Lang::Tsx => "TSX",
            Lang::JavaScript => "JavaScript",
            Lang::Python => "Python",
            Lang::Go => "Go",
            Lang::Java => "Java",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
            Lang::Kotlin => "Kotlin",
            Lang::Ruby => "Ruby",
            Lang::Php => "PHP",
            Lang::Swift => "Swift",
            Lang::Bash => "Bash",
            Lang::Json => "JSON",
            Lang::Toml => "TOML",
            Lang::Yaml => "YAML",
            Lang::Markdown => "Markdown",
        }
    }

    /// Whether this build includes the grammar (Cargo feature of the same name).
    pub fn is_available(self) -> bool {
        self.grammar().is_some()
    }

    /// The tree-sitter grammar; `None` when its Cargo feature is off. Cheap: a pointer
    /// to the grammar's static tables, nothing is cached.
    pub(crate) fn grammar(self) -> Option<Language> {
        match self {
            Lang::Rust => rust(),
            Lang::TypeScript => typescript(),
            Lang::Tsx => tsx(),
            Lang::JavaScript => javascript(),
            Lang::Python => python(),
            Lang::Go => go(),
            Lang::Java => java(),
            Lang::C => c(),
            Lang::Cpp => cpp(),
            Lang::CSharp => csharp(),
            Lang::Kotlin => kotlin(),
            Lang::Ruby => ruby(),
            Lang::Php => php(),
            Lang::Swift => swift(),
            Lang::Bash => bash(),
            Lang::Json => json(),
            Lang::Toml => toml(),
            Lang::Yaml => yaml(),
            Lang::Markdown => markdown(),
        }
    }

    pub(crate) fn spec(self) -> &'static LangSpec {
        match self {
            Lang::Rust => &tables::RUST,
            Lang::TypeScript | Lang::Tsx => &tables::TYPESCRIPT,
            Lang::JavaScript => &tables::JAVASCRIPT,
            Lang::Python => &tables::PYTHON,
            Lang::Go => &tables::GO,
            Lang::Java => &tables::JAVA,
            Lang::C => &tables::C,
            Lang::Cpp => &tables::CPP,
            Lang::CSharp => &tables::CSHARP,
            Lang::Kotlin => &tables::KOTLIN,
            Lang::Ruby => &tables::RUBY,
            Lang::Php => &tables::PHP,
            Lang::Swift => &tables::SWIFT,
            Lang::Bash => &tables::BASH,
            Lang::Json => &tables::JSON,
            Lang::Toml => &tables::TOML,
            Lang::Yaml => &tables::YAML,
            Lang::Markdown => &tables::MARKDOWN,
        }
    }
}

/// `fn $name() -> Option<Language>`: the grammar when `$feature` is on, else `None`.
macro_rules! grammar_fn {
    ($name:ident, $feature:literal, $language:expr) => {
        #[cfg(feature = $feature)]
        fn $name() -> Option<Language> {
            Some(Language::new($language))
        }
        #[cfg(not(feature = $feature))]
        fn $name() -> Option<Language> {
            None
        }
    };
}

grammar_fn!(rust, "rust", tree_sitter_rust::LANGUAGE);
grammar_fn!(
    typescript,
    "typescript",
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT
);
grammar_fn!(tsx, "typescript", tree_sitter_typescript::LANGUAGE_TSX);
grammar_fn!(javascript, "javascript", tree_sitter_javascript::LANGUAGE);
grammar_fn!(python, "python", tree_sitter_python::LANGUAGE);
grammar_fn!(go, "go", tree_sitter_go::LANGUAGE);
grammar_fn!(java, "java", tree_sitter_java::LANGUAGE);
grammar_fn!(c, "c", tree_sitter_c::LANGUAGE);
grammar_fn!(cpp, "cpp", tree_sitter_cpp::LANGUAGE);
grammar_fn!(csharp, "csharp", tree_sitter_c_sharp::LANGUAGE);
grammar_fn!(kotlin, "kotlin", tree_sitter_kotlin_ng::LANGUAGE);
grammar_fn!(ruby, "ruby", tree_sitter_ruby::LANGUAGE);
grammar_fn!(php, "php", tree_sitter_php::LANGUAGE_PHP);
grammar_fn!(swift, "swift", tree_sitter_swift::LANGUAGE);
grammar_fn!(bash, "bash", tree_sitter_bash::LANGUAGE);
grammar_fn!(json, "json", tree_sitter_json::LANGUAGE);
grammar_fn!(toml, "toml", tree_sitter_toml_ng::LANGUAGE);
grammar_fn!(yaml, "yaml", tree_sitter_yaml::LANGUAGE);
grammar_fn!(markdown, "markdown", tree_sitter_md::LANGUAGE);
