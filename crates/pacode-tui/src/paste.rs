//! Bracketed text paste and clipboard image paste handling.

use std::time::Instant;

use pacode_types::ToastLevel;

use crate::clipboard_read::{
    ClipboardImageResult, ClipboardPlatform, CommandRunner, SystemCommandRunner, read_image_data,
    read_text_data,
};
use crate::keys::Action;
use crate::state::input::char_to_byte_index;
use crate::state::vim::{VimMode, VimState};
use crate::state::{AppState, Focus, Overlay};

#[cfg(test)]
#[path = "paste_tests.rs"]
mod paste_tests;

/// Maximum characters accepted in a single text paste into the prompt.
/// Accidental pastes of giant files (e.g. 50 MB logs) are truncated to this cap.
pub const PASTE_MAX_CHARS: usize = 65_536;

/// Maximum length for a filter box query pasted from clipboard.
pub const FILTER_MAX_CHARS: usize = 512;

/// Handles `ctrl+v` / `ctrl+shift+v`: an image from the clipboard when there is one,
/// otherwise the clipboard text, read through the same helper programs.
pub fn handle_paste_clipboard(state: &mut AppState, now: Instant) -> Vec<Action> {
    handle_paste_clipboard_with_runner(
        state,
        &SystemCommandRunner,
        ClipboardPlatform::current(),
        now,
    )
}

/// `handle_paste_clipboard` with an injectable command runner for tests.
pub fn handle_paste_clipboard_with_runner(
    state: &mut AppState,
    runner: &dyn CommandRunner,
    platform: ClipboardPlatform,
    now: Instant,
) -> Vec<Action> {
    if let ClipboardImageResult::Image(_) = read_image_data(runner, platform) {
        return handle_paste_image_with_runner(state, true, runner, platform, now);
    }
    match read_text_data(runner, platform) {
        Some(text) => handle_paste_with_runner(state, text, runner, platform, now),
        None => {
            state.push_toast(
                ToastLevel::Info,
                "Clipboard is empty".to_string(),
                None,
                now,
            );
            vec![]
        }
    }
}

/// Handles a bracketed paste event from the terminal.
pub fn handle_paste(state: &mut AppState, text: String, now: Instant) -> Vec<Action> {
    handle_paste_with_runner(
        state,
        text,
        &SystemCommandRunner,
        ClipboardPlatform::current(),
        now,
    )
}

/// Handles a bracketed paste event with an injectable command runner for tests.
pub fn handle_paste_with_runner(
    state: &mut AppState,
    text: String,
    runner: &dyn CommandRunner,
    platform: ClipboardPlatform,
    now: Instant,
) -> Vec<Action> {
    if text.is_empty() {
        // When bracketed paste carries empty text (e.g. ctrl+shift+v with an image in clipboard),
        // try the image clipboard.
        return handle_paste_image_with_runner(state, false, runner, platform, now);
    }

    match &mut state.focus {
        Focus::Overlay(Overlay::ModelPicker { query, index })
        | Focus::Overlay(Overlay::LoginPicker { query, index }) => {
            paste_into_filter(query, index, &text);
            state.dirty = true;
            vec![]
        }
        Focus::Overlay(Overlay::SessionPicker { query, index }) => {
            paste_into_filter(query, index, &text);
            state.dirty = true;
            vec![]
        }
        Focus::Overlay(Overlay::ConfigPicker) => {
            if let Some(ref mut edit) = state.config_view.editing {
                let flattened = flatten_single_line(&text);
                let count = flattened.chars().count();
                let byte_idx = char_to_byte_index(&edit.buffer, edit.cursor);
                edit.buffer.insert_str(byte_idx, &flattened);
                edit.cursor += count;
                state.config_view.error = None;
            } else {
                let flattened = flatten_single_line(&text);
                state.config_view.query.push_str(&flattened);
                state.config_view.selected = 0;
                state.config_view.scroll_offset = 0;
                state.config_view.error = None;
            }
            state.dirty = true;
            vec![]
        }
        Focus::Overlay(_) => {
            // Overlays without a text/filter box (EffortPicker, ModePicker, Files, McpPicker, etc.)
            // ignore pastes so text does not leak into the hidden prompt.
            vec![]
        }
        Focus::Normal | Focus::Panel { .. } | Focus::SelectAgent { .. } | Focus::BgList { .. } => {
            if matches!(
                state.focus,
                Focus::SelectAgent { .. } | Focus::BgList { .. }
            ) {
                state.focus = Focus::Normal;
            }
            paste_into_prompt(state, text, now);
            state.dirty = true;
            vec![]
        }
    }
}

/// Pastes text into an overlay's single-line filter box.
fn paste_into_filter(query: &mut String, index: &mut usize, text: &str) {
    let flattened = flatten_single_line(text);
    let available = FILTER_MAX_CHARS.saturating_sub(query.chars().count());
    let to_insert: String = flattened.chars().take(available).collect();
    query.push_str(&to_insert);
    *index = 0;
}

/// Flattens multi-line text into a single line suitable for filter boxes.
fn flatten_single_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pastes text into `state.input`, enforcing size caps and respecting vim mode.
fn paste_into_prompt(state: &mut AppState, text: String, now: Instant) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let total_chars = normalized.chars().count();

    let paste_content = if total_chars > PASTE_MAX_CHARS {
        log::warn!("pasted text truncated from {total_chars} to {PASTE_MAX_CHARS} chars");
        state.push_toast(
            ToastLevel::Warn,
            format!("Pasted text truncated to {PASTE_MAX_CHARS} chars"),
            Some(format!("Original was {total_chars} chars")),
            now,
        );
        normalized.chars().take(PASTE_MAX_CHARS).collect()
    } else {
        normalized
    };

    if state.config.ui.vim && state.focus == Focus::Normal {
        apply_vim_paste(&mut state.vim, &mut state.input, &paste_content);
    } else {
        state.input.insert_str(&paste_content);
    }

    state.cleanup_pasted_images();
}

/// Applies text paste in vim mode.
fn apply_vim_paste(vim: &mut VimState, input: &mut crate::state::InputState, text: &str) {
    match vim.mode {
        VimMode::Insert => {
            push_vim_undo(vim, input, input.cursor);
            input.insert_str(text);
        }
        VimMode::Normal => {
            push_vim_undo(vim, input, input.cursor);
            vim.pending.clear();
            vim.count = None;
            input.insert_str(text);
            input.cursor = clamp_vim_cursor(&input.text, input.cursor);
        }
        VimMode::Visual => {
            let anchor = vim.visual_anchor.unwrap_or(input.cursor);
            let chars: Vec<char> = input.text.chars().collect();
            let (start, end) = (
                anchor.min(input.cursor),
                (anchor.max(input.cursor) + 1).min(chars.len()),
            );
            vim.count = None;
            push_vim_undo(vim, input, start);
            replace_char_range(input, start, end, text);
            input.cursor = clamp_vim_cursor(&input.text, start + text.chars().count());
            vim.mode = VimMode::Normal;
            vim.visual_anchor = None;
        }
    }
}

fn push_vim_undo(vim: &mut VimState, input: &crate::state::InputState, cursor: usize) {
    if vim.undo_stack.len() >= 32 {
        vim.undo_stack.pop_front();
    }
    vim.undo_stack.push_back((input.text.clone(), cursor));
}

fn replace_char_range(input: &mut crate::state::InputState, start: usize, end: usize, rep: &str) {
    let sb = char_to_byte_index(&input.text, start);
    let eb = char_to_byte_index(&input.text, end);
    input.text.replace_range(sb..eb, rep);
}

fn clamp_vim_cursor(text: &str, cursor: usize) -> usize {
    let count = text.chars().count();
    if count == 0 {
        0
    } else {
        cursor.min(count.saturating_sub(1))
    }
}

/// Handles pasting an image from the system clipboard.
pub fn handle_paste_image(state: &mut AppState, explicit: bool, now: Instant) -> Vec<Action> {
    handle_paste_image_with_runner(
        state,
        explicit,
        &SystemCommandRunner,
        ClipboardPlatform::current(),
        now,
    )
}

/// Handles pasting an image with an injectable command runner for tests.
pub fn handle_paste_image_with_runner(
    state: &mut AppState,
    explicit: bool,
    runner: &dyn CommandRunner,
    platform: ClipboardPlatform,
    now: Instant,
) -> Vec<Action> {
    // If an overlay is active, pasting an image into it is invalid.
    if matches!(state.focus, Focus::Overlay(_)) {
        log::warn!("paste image rejected: overlay active");
        if explicit {
            state.push_toast(
                ToastLevel::Warn,
                "Cannot paste image".to_string(),
                Some("Close the open overlay before pasting images".to_string()),
                now,
            );
        }
        return vec![];
    }

    let result = read_image_data(runner, platform);

    match result {
        ClipboardImageResult::Image(bytes) => match state.pasted_images.create_image_file(&bytes) {
            Ok(path) => {
                let token = format!("@{} ", path.display());
                if state.config.ui.vim && state.focus == Focus::Normal {
                    apply_vim_paste(&mut state.vim, &mut state.input, &token);
                } else {
                    state.input.insert_str(&token);
                }
                state.push_toast(
                        ToastLevel::Info,
                        "Image pasted".to_string(),
                        Some(
                            "Preview shown inline. Daemon attaches image as metadata (name & size), not pixels (v1 spec §18.5)."
                                .to_string(),
                        ),
                        now,
                    );
                state.dirty = true;
            }
            Err(err) => {
                log::warn!("clipboard image paste failed to save file: {err}");
                state.push_toast(
                    ToastLevel::Error,
                    "Failed to save pasted image".to_string(),
                    Some(err.to_string()),
                    now,
                );
            }
        },
        ClipboardImageResult::NoImage => {
            log::debug!("clipboard image paste: no image found in clipboard");
            if explicit {
                state.push_toast(
                    ToastLevel::Info,
                    "No image found in clipboard".to_string(),
                    None,
                    now,
                );
            }
        }
        ClipboardImageResult::HelperMissing(msg) => {
            log::warn!("clipboard image paste failed: helper missing: {msg}");
            if explicit {
                state.push_toast(
                    ToastLevel::Warn,
                    "Clipboard helper missing".to_string(),
                    Some(msg),
                    now,
                );
            }
        }
    }

    vec![]
}

/// Cleans up any unreferenced pasted images from disk.
pub fn cleanup_unreferenced_images(state: &mut AppState) {
    state.cleanup_pasted_images();
}
