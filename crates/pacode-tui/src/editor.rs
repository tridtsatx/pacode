//! External $EDITOR integration for editing the prompt in a full editor.

#[cfg(test)]
#[path = "editor_tests.rs"]
mod editor_tests;

use std::time::Instant;

use pacode_types::ToastLevel;

use crate::state::AppState;
use crate::terminal;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("failed to create temp file: {0}")]
    CreateTemp(#[source] std::io::Error),
    #[error("failed to write prompt to temp file: {0}")]
    WriteTemp(#[source] std::io::Error),
    #[error("failed to spawn editor '{cmd}': {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to wait for editor '{cmd}': {source}")]
    Wait {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("editor '{cmd}' exited with status {status}")]
    NonZeroExit {
        cmd: String,
        status: std::process::ExitStatus,
    },
    #[error("failed to read temp file: {0}")]
    ReadTemp(#[source] std::io::Error),
    #[error("terminal error: {0}")]
    Terminal(#[source] std::io::Error),
}

/// Resolves the editor command and arguments from environment variables:
/// `$VISUAL`, then `$EDITOR`, falling back to `vi`.
pub fn resolve_editor() -> (String, Vec<String>) {
    resolve_editor_from(|var| std::env::var(var).ok())
}

/// Pure helper for editor resolution with injectable environment lookup.
pub fn resolve_editor_from(
    mut get_env: impl FnMut(&str) -> Option<String>,
) -> (String, Vec<String>) {
    let raw = get_env("VISUAL")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| get_env("EDITOR").filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_string());

    let mut tokens = raw.split_whitespace();
    let program = tokens.next().unwrap_or("vi").to_string();
    let args: Vec<String> = tokens.map(String::from).collect();
    (program, args)
}

/// Trims exactly one trailing newline (`\r\n` or `\n`) if present, preserving interior blank lines.
pub fn trim_single_trailing_newline(text: &str) -> &str {
    if let Some(stripped) = text.strip_suffix("\r\n") {
        stripped
    } else if let Some(stripped) = text.strip_suffix('\n') {
        stripped
    } else {
        text
    }
}

/// Runs a command on a temp file containing `initial_text`, reads it back, deletes it,
/// and returns the updated text with at most one trailing newline trimmed.
pub fn edit_text_with(
    program: &str,
    args: &[String],
    initial_text: &str,
) -> Result<String, EditorError> {
    let temp_file = tempfile::Builder::new()
        .suffix(".md")
        .tempfile_in(std::env::temp_dir())
        .map_err(EditorError::CreateTemp)?;

    let temp_path = temp_file.into_temp_path();
    let path = temp_path.to_path_buf();

    std::fs::write(&path, initial_text).map_err(EditorError::WriteTemp)?;

    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.arg(&path);

    log::debug!("spawning editor '{program}' with args {args:?}");
    let mut child = cmd.spawn().map_err(|e| {
        log::warn!("failed to spawn editor '{program}': {e}");
        EditorError::Spawn {
            cmd: program.to_string(),
            source: e,
        }
    })?;

    let status = child.wait().map_err(|e| {
        log::warn!("failed to wait for editor '{program}': {e}");
        EditorError::Wait {
            cmd: program.to_string(),
            source: e,
        }
    })?;

    log::debug!("editor '{program}' exited with status {status}");

    if !status.success() {
        return Err(EditorError::NonZeroExit {
            cmd: program.to_string(),
            status,
        });
    }

    let content = std::fs::read_to_string(&path).map_err(EditorError::ReadTemp)?;

    let _ = temp_path.close();

    let trimmed = trim_single_trailing_newline(&content);
    Ok(trimmed.to_string())
}

/// Edits `initial_text` in the resolved editor while suspending the TUI.
pub fn edit_prompt(mouse: bool, initial_text: &str) -> Result<String, EditorError> {
    let (program, args) = resolve_editor();
    terminal::run_suspended(mouse, || edit_text_with(&program, &args, initial_text))
        .map_err(EditorError::Terminal)?
}

/// Opens the editor to edit `state.input.text`. Updates state on success, shows toast on error.
pub fn open_editor(state: &mut AppState) {
    let mouse = state.config.ui.mouse;
    let initial = state.input.text.clone();
    match edit_prompt(mouse, &initial) {
        Ok(new_text) => {
            state.input.text = new_text;
            state.input.end();
            state.input.input_scroll = 0;
            state.dirty = true;
        }
        Err(err) => {
            state.push_toast(
                ToastLevel::Error,
                "Editor error".to_string(),
                Some(err.to_string()),
                Instant::now(),
            );
        }
    }
}
