//! Simple pipelines of supported utilities: `wc -l src/*.rs | sort -n`.
//!
//! Quoting follows the shell rules used by the risk tokenizer (single quotes, double quotes
//! with `\" \\ \$ \``, backslash). Redirection, `;`, `&`, `||`, subshells, command and
//! variable expansion, tilde and newlines are rejected. Globs are expanded only when asked.

use std::path::Path;

use crate::{UTILS, UtilOutput};

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod pipeline_tests;

const MAX_GLOB_RESULTS: usize = 2000;
const MAX_STAGES: usize = 16;

/// One utility invocation with its arguments (globs already expanded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub name: String,
    pub args: Vec<String>,
}

/// Why a command line is not a supported pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("{0}")]
    Syntax(String),
    #[error("unsupported utility `{0}`; supported: {list}", list = UTILS.join(" "))]
    UnknownUtility(String),
    #[error("{0}")]
    Glob(String),
}

struct RawWord {
    text: String,
    glob: bool,
}

const UNSUPPORTED_HINT: &str = "is not supported here (no redirection, `;`, `&`, `||`, subshells, command/variable expansion, `~` or multi-line commands)";

#[allow(unused_assignments)]
fn lex(command: &str) -> Result<Vec<Vec<RawWord>>, PipelineError> {
    let unsupported = |what: &str| PipelineError::Syntax(format!("{what} {UNSUPPORTED_HINT}"));
    let mut stages: Vec<Vec<RawWord>> = vec![Vec::new()];
    let mut text = String::new();
    let mut has_content = false;
    let mut glob = false;
    let mut chars = command.chars().peekable();

    macro_rules! flush {
        () => {
            if has_content {
                let word = RawWord {
                    text: std::mem::take(&mut text),
                    glob,
                };
                if let Some(stage) = stages.last_mut() {
                    stage.push(word);
                }
                has_content = false;
                glob = false;
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => flush!(),
            '\n' | '\r' => return Err(unsupported("a multi-line command")),
            '\'' => {
                has_content = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(q) => text.push(q),
                        None => {
                            return Err(PipelineError::Syntax("unterminated single quote".into()));
                        }
                    }
                }
            }
            '"' => {
                has_content = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => text.push(e),
                            Some(other) => {
                                text.push('\\');
                                text.push(other);
                            }
                            None => {
                                return Err(PipelineError::Syntax(
                                    "unterminated double quote".into(),
                                ));
                            }
                        },
                        Some('$') => return Err(unsupported("variable or command expansion")),
                        Some('`') => return Err(unsupported("command substitution")),
                        Some(q) => text.push(q),
                        None => {
                            return Err(PipelineError::Syntax("unterminated double quote".into()));
                        }
                    }
                }
            }
            '\\' => match chars.next() {
                Some(next) => {
                    has_content = true;
                    text.push(next);
                }
                None => return Err(PipelineError::Syntax("trailing backslash".into())),
            },
            '|' => {
                if chars.peek() == Some(&'|') {
                    return Err(unsupported("`||`"));
                }
                flush!();
                if stages.last().is_some_and(|s| s.is_empty()) {
                    return Err(PipelineError::Syntax("empty command before `|`".into()));
                }
                if stages.len() >= MAX_STAGES {
                    return Err(PipelineError::Syntax(format!(
                        "at most {MAX_STAGES} pipeline stages"
                    )));
                }
                stages.push(Vec::new());
            }
            '>' | '<' => return Err(unsupported("redirection")),
            '&' => return Err(unsupported("`&`")),
            ';' => return Err(unsupported("`;`")),
            '(' | ')' => return Err(unsupported("a subshell")),
            '`' => return Err(unsupported("command substitution")),
            '$' => return Err(unsupported("variable or command expansion")),
            '~' if !has_content => return Err(unsupported("tilde expansion")),
            '*' | '?' | '[' => {
                has_content = true;
                glob = true;
                text.push(c);
            }
            other => {
                has_content = true;
                text.push(other);
            }
        }
    }
    flush!();
    if stages.last().is_some_and(|s| s.is_empty()) {
        return Err(PipelineError::Syntax(if stages.len() > 1 {
            "empty command after `|`".to_string()
        } else {
            "empty command".to_string()
        }));
    }
    Ok(stages)
}

fn join_path(prefix: &str, component: &str) -> String {
    if prefix.is_empty() {
        component.to_string()
    } else if prefix == "/" {
        format!("/{component}")
    } else {
        format!("{prefix}/{component}")
    }
}

fn has_glob_chars(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// Expand a glob relative to `cwd`; results are sorted and keep the pattern's path style.
pub fn expand_glob(pattern: &str, cwd: &Path) -> Result<Vec<String>, PipelineError> {
    let mut candidates: Vec<String> = vec![if pattern.starts_with('/') {
        "/".to_string()
    } else {
        String::new()
    }];
    for component in pattern.split('/').filter(|c| !c.is_empty()) {
        let mut next = Vec::new();
        if has_glob_chars(component) {
            let matcher = globset::GlobBuilder::new(component)
                .literal_separator(true)
                .build()
                .map_err(|e| PipelineError::Glob(format!("invalid glob '{pattern}': {e}")))?
                .compile_matcher();
            for dir in &candidates {
                let real = if dir.is_empty() {
                    cwd.to_path_buf()
                } else if Path::new(dir).is_absolute() {
                    Path::new(dir).to_path_buf()
                } else {
                    cwd.join(dir)
                };
                let Ok(read_dir) = std::fs::read_dir(real) else {
                    continue;
                };
                let mut names: Vec<String> = read_dir
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| component.starts_with('.') || !n.starts_with('.'))
                    .filter(|n| matcher.is_match(n.as_str()))
                    .collect();
                names.sort();
                for name in names {
                    next.push(join_path(dir, &name));
                }
            }
        } else {
            for dir in &candidates {
                next.push(join_path(dir, component));
            }
        }
        if next.len() > MAX_GLOB_RESULTS {
            return Err(PipelineError::Glob(format!(
                "glob '{pattern}' matches more than {MAX_GLOB_RESULTS} paths"
            )));
        }
        candidates = next;
    }
    candidates.retain(|c| {
        let real = if Path::new(c).is_absolute() {
            Path::new(c).to_path_buf()
        } else {
            cwd.join(c)
        };
        std::fs::symlink_metadata(real).is_ok()
    });
    if candidates.is_empty() {
        return Err(PipelineError::Glob(format!(
            "no match for glob '{pattern}'"
        )));
    }
    Ok(candidates)
}

/// Parse `command` into stages. With `expand_globs` false any glob is an error.
pub fn parse(command: &str, cwd: &Path, expand_globs: bool) -> Result<Vec<Stage>, PipelineError> {
    let raw = lex(command)?;
    let mut stages = Vec::new();
    for words in raw {
        let mut iter = words.into_iter();
        let Some(first) = iter.next() else {
            return Err(PipelineError::Syntax("empty command".into()));
        };
        if first.glob || !UTILS.contains(&first.text.as_str()) {
            return Err(PipelineError::UnknownUtility(first.text));
        }
        let mut args = Vec::new();
        for word in iter {
            if !word.glob {
                args.push(word.text);
            } else if expand_globs {
                args.extend(expand_glob(&word.text, cwd)?);
            } else {
                return Err(PipelineError::Glob(format!(
                    "glob '{}' is not supported here",
                    word.text
                )));
            }
        }
        stages.push(Stage {
            name: first.text,
            args,
        });
    }
    Ok(stages)
}

/// Result of running a whole pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    /// Exit code of the last stage (shell semantics without `pipefail`).
    pub code: i32,
    /// Rightmost failing stage as `(name, code)`, if any stage exited non-zero.
    pub failure: Option<(String, i32)>,
}

impl PipelineOutput {
    /// Whether the failure is only `diff` reporting differences (exit 1).
    pub fn is_benign(&self) -> bool {
        match &self.failure {
            None => true,
            Some((name, code)) => name == "diff" && *code == 1,
        }
    }
}

/// Run stages in order, feeding each stage's stdout to the next.
pub fn run_pipeline(stages: &[Stage], stdin: Option<&[u8]>, cwd: &Path) -> PipelineOutput {
    let mut data: Option<Vec<u8>> = stdin.map(<[u8]>::to_vec);
    let mut result = PipelineOutput::default();
    for stage in stages {
        let out: UtilOutput = crate::run(&stage.name, &stage.args, data.as_deref(), cwd);
        result.stderr.push_str(&out.stderr);
        result.code = out.code;
        if out.code != 0 {
            result.failure = Some((stage.name.clone(), out.code));
        }
        data = Some(out.stdout);
    }
    result.stdout = data.unwrap_or_default();
    result
}
