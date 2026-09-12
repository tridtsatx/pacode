//! Language detection: a file extension for diffs and previews, an info string
//! for fenced code blocks. Unknown languages are `None`, never an error.

#[cfg(test)]
#[path = "lang_tests.rs"]
mod lang_tests;

/// The eight languages the line lexer knows. `TypeScript` covers the whole
/// JS/TS family (`.js .jsx .ts .tsx .mjs .cjs .mts .cts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Json,
    Toml,
    Yaml,
    Bash,
    Markdown,
}

/// Aliases accepted both as file extensions and as fence info strings.
const ALIASES: &[(&[&str], Lang)] = &[
    (&["rs", "rust"], Lang::Rust),
    (
        &["py", "pyi", "pyw", "py3", "pyx", "python", "python3"],
        Lang::Python,
    ),
    (
        &[
            "ts",
            "tsx",
            "mts",
            "cts",
            "typescript",
            "js",
            "jsx",
            "mjs",
            "cjs",
            "javascript",
            "node",
        ],
        Lang::TypeScript,
    ),
    (&["json", "jsonc", "json5", "jsonl"], Lang::Json),
    (&["toml"], Lang::Toml),
    (&["yaml", "yml"], Lang::Yaml),
    (
        &["sh", "bash", "zsh", "ksh", "shell", "bashrc", "zshrc"],
        Lang::Bash,
    ),
    (&["md", "mdx", "markdown", "mdown", "mkd"], Lang::Markdown),
];

/// Dotfiles whose "extension" is not an extension at all (`.bashrc`), and
/// well-known names. Checked before the extension rule.
const FILE_NAMES: &[(&str, Lang)] = &[
    (".bashrc", Lang::Bash),
    (".bash_profile", Lang::Bash),
    (".bash_logout", Lang::Bash),
    (".zshrc", Lang::Bash),
    (".zprofile", Lang::Bash),
    (".profile", Lang::Bash),
    ("bashrc", Lang::Bash),
    ("cargo.lock", Lang::Toml),
    ("pipfile", Lang::Toml),
];

fn match_alias(word: &str) -> Option<Lang> {
    ALIASES
        .iter()
        .find_map(|(names, lang)| names.contains(&word).then_some(*lang))
}

/// Detect the language of `path_or_info`: a path (`src/main.rs`,
/// `b/src/main.rs`, `a/x` diff prefixes included) or a fence info string
/// (`rust`, `python3`, `ts`, `markdown`). Returns `None` for anything unknown —
/// the caller renders without highlighting.
pub fn detect_lang(path_or_info: &str) -> Option<Lang> {
    // Fence info strings carry flags after the language (`rust,no_run`,
    // `python {1,2}`); the language is the first bare token.
    let first = path_or_info
        .trim()
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()
        .unwrap_or("");
    if first.is_empty() {
        return None;
    }
    let lower = first.to_ascii_lowercase();

    // A path: take the file name, honour dotfiles and known names, then the
    // extension. `b/src/main.rs` works because only the last component counts.
    let file_name = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    if let Some(lang) = FILE_NAMES
        .iter()
        .find_map(|(name, lang)| (*name == file_name).then_some(*lang))
    {
        return Some(lang);
    }
    if let Some(ext) = file_name.rsplit_once('.').map(|(_, ext)| ext)
        && let Some(lang) = match_alias(ext)
    {
        return Some(lang);
    }

    // Not a path (or an unknown one): try the token itself as an alias, so
    // fence info strings like ```py or a bare `bash` resolve too.
    match_alias(file_name)
}
