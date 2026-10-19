//! Language detection and tree-sitter language resolution.

use std::fmt;
use std::path::Path;

/// A code language with a linked grammar and a symbol query. Markup and config
/// formats (html/css/yaml/toml/json/…) are intentionally out of scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    C,
    Cpp,
    Java,
    CSharp,
    Ruby,
    Php,
    Swift,
    Kotlin,
    Scala,
    Lua,
    Bash,
}

impl Language {
    pub const ALL: &'static [Language] = &[
        Language::Rust,
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::Go,
        Language::C,
        Language::Cpp,
        Language::Java,
        Language::CSharp,
        Language::Ruby,
        Language::Php,
        Language::Swift,
        Language::Kotlin,
        Language::Scala,
        Language::Lua,
        Language::Bash,
    ];

    /// Stable lowercase name — shown in output headers and accepted by
    /// [`Language::from_name`] for the `lang` override.
    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Go => "go",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Java => "java",
            Language::CSharp => "csharp",
            Language::Ruby => "ruby",
            Language::Php => "php",
            Language::Swift => "swift",
            Language::Kotlin => "kotlin",
            Language::Scala => "scala",
            Language::Lua => "lua",
            Language::Bash => "bash",
        }
    }

    /// Explicit language name (the `lang` tool argument). Accepts the canonical
    /// [`Language::name`] plus the obvious aliases (`js`, `ts`, `c++`, `sh`, …).
    pub fn from_name(name: &str) -> Option<Language> {
        match name.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(Language::Rust),
            "python" | "py" => Some(Language::Python),
            "javascript" | "js" | "jsx" => Some(Language::JavaScript),
            "typescript" | "ts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "go" | "golang" => Some(Language::Go),
            "c" | "h" => Some(Language::C),
            "cpp" | "c++" | "cxx" | "cc" => Some(Language::Cpp),
            "java" => Some(Language::Java),
            "csharp" | "c#" | "cs" => Some(Language::CSharp),
            "ruby" | "rb" => Some(Language::Ruby),
            "php" => Some(Language::Php),
            "swift" => Some(Language::Swift),
            "kotlin" | "kt" => Some(Language::Kotlin),
            "scala" => Some(Language::Scala),
            "lua" => Some(Language::Lua),
            "bash" | "sh" | "shell" | "zsh" => Some(Language::Bash),
            _ => None,
        }
    }

    /// Guess the language from a path's extension or special file name
    /// (`Makefile`, `CMakeLists.txt`, `.bashrc`, …).
    pub fn from_path(path: &Path) -> Option<Language> {
        let name = path.file_name()?.to_str()?;
        match name {
            "Makefile" | "makefile" | "GNUmakefile" | "CMakeLists.txt" | "Dockerfile"
            | "dockerfile" | "Containerfile" | "containerfile" => return None,
            _ => {}
        }
        // Shell rc files have no extension (`.bashrc` reports `bashrc` as the
        // "extension" of a dotfile only when there is more than one dot).
        let stem = name.strip_prefix('.').unwrap_or(name);
        if matches!(
            stem,
            "bashrc"
                | "bash_profile"
                | "bash_login"
                | "bash_logout"
                | "bash_aliases"
                | "zshrc"
                | "zshenv"
                | "zprofile"
                | "profile"
                | "shrc"
                | "kshrc"
        ) {
            return Some(Language::Bash);
        }
        match path.extension().and_then(|e| e.to_str())? {
            "rs" => Some(Language::Rust),
            "py" | "pyi" | "py3" => Some(Language::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "go" => Some(Language::Go),
            "c" | "h" => Some(Language::C),
            "cpp" | "cxx" | "cc" | "c++" | "hpp" | "hh" | "hxx" | "cu" | "cuh" => {
                Some(Language::Cpp)
            }
            "java" => Some(Language::Java),
            "cs" => Some(Language::CSharp),
            "rb" | "rbw" | "gemspec" => Some(Language::Ruby),
            "php" | "php3" | "php4" | "php5" | "phtml" => Some(Language::Php),
            "swift" => Some(Language::Swift),
            "kt" | "kts" => Some(Language::Kotlin),
            "scala" | "sc" | "sbt" => Some(Language::Scala),
            "lua" => Some(Language::Lua),
            "sh" | "bash" | "zsh" | "ksh" => Some(Language::Bash),
            _ => None,
        }
    }

    /// The tree-sitter grammar for this language.
    pub fn ts_language(self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::Php => tree_sitter_php::LANGUAGE_PHP_ONLY.into(),
            Language::Swift => tree_sitter_swift::LANGUAGE.into(),
            Language::Kotlin => tree_sitter_kotlin_sg::LANGUAGE.into(),
            Language::Scala => tree_sitter_scala::LANGUAGE.into(),
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
