//! Bracketed text paste and clipboard image paste handling.

use std::time::Instant;

use pacode_types::ToastLevel;

use crate::clipboard_read::{
    ClipboardImageResult, ClipboardPlatform, CommandRunner, IMAGE_PASTE_MAX_BYTES,
    SystemCommandRunner, read_image_data, read_text_data,
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
    match read_image_data(runner, platform) {
        ClipboardImageResult::Image(bytes) => {
            if matches!(state.focus, Focus::Overlay(_)) {
                log::warn!("paste image rejected: overlay active");
                state.push_toast(
                    ToastLevel::Warn,
                    "Cannot paste image".to_string(),
                    Some("Close the open overlay before pasting images".to_string()),
                    now,
                );
                return vec![];
            }
            handle_paste_image_bytes(state, bytes, now)
        }
        _ => match read_text_data(runner, platform) {
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
        },
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

    // A menu accepts pasted text only where text input is live: an Edit-mode
    // buffer or a filter line (§3.2). It is checked before the match —
    // `view_for` needs a shared `&state`, which the `&mut` arm borrow blocks.
    let mut menu_remote_ms = None;
    let menu_takes_paste = if let Focus::Overlay(Overlay::Menu { stack, ui }) = &state.focus {
        match &ui.mode {
            crate::ui::menu::MenuMode::Edit { .. } => true,
            _ => {
                stack.current().is_some() && {
                    let view = crate::ui::menu::view_for(stack, state, ui);
                    if let Some(crate::ui::menu::FilterMode::Remote { debounce_ms }) = view.filter {
                        menu_remote_ms = Some(debounce_ms);
                    }
                    crate::ui::menu::shows_filter(&view)
                }
            }
        }
    } else {
        false
    };

    match &mut state.focus {
        Focus::Overlay(Overlay::ModelPalette { palette }) => {
            palette.push_str(&flatten_single_line(&text, FILTER_MAX_CHARS));
            state.dirty = true;
            vec![]
        }
        Focus::Overlay(Overlay::Menu { ui, .. }) if menu_takes_paste => {
            let flattened = flatten_single_line(&text, PASTE_MAX_CHARS);
            match &mut ui.mode {
                crate::ui::menu::MenuMode::Edit { buffer, cursor } => {
                    let count = flattened.chars().count();
                    let byte_idx = char_to_byte_index(buffer, *cursor);
                    buffer.insert_str(byte_idx, &flattened);
                    *cursor += count;
                }
                _ => {
                    let available = FILTER_MAX_CHARS.saturating_sub(ui.query.chars().count());
                    let to_insert: String = flattened.chars().take(available).collect();
                    ui.query.push_str(&to_insert);
                    ui.cursor = crate::ui::menu::RowId::default();
                    // A remote filter owes the daemon the pasted query — the
                    // same debounce keystrokes arm (§3.2).
                    if !to_insert.is_empty()
                        && let Some(ms) = menu_remote_ms
                    {
                        ui.remote_at = Some(now + std::time::Duration::from_millis(ms));
                    }
                }
            }
            state.dirty = true;
            vec![]
        }
        Focus::Overlay(_) => {
            // Overlays without a text/filter box (EffortPicker, ModePicker,
            // RailOverlay, a menu without a live filter) ignore pastes so text
            // does not leak into the hidden prompt.
            vec![]
        }
        Focus::Normal | Focus::Panel { .. } | Focus::SelectAgent { .. } => {
            if matches!(state.focus, Focus::SelectAgent { .. }) {
                state.focus = Focus::Normal;
            }
            paste_into_prompt(state, text, now);
            state.dirty = true;
            vec![]
        }
    }
}

fn flatten_single_line(text: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut kept = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            if kept == cap {
                break;
            }
            out.push(' ');
            kept += 1;
        }
        for ch in line.chars() {
            if kept == cap {
                break;
            }
            out.push(ch);
            kept += 1;
        }
    }
    out
}

fn normalize_paste_prefix(text: &str, cap: usize) -> (String, usize, bool) {
    let mut out = String::new();
    let mut kept = 0usize;
    let mut total = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let ch = if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            '\n'
        } else {
            ch
        };
        total += 1;
        if kept < cap {
            out.push(ch);
            kept += 1;
        }
    }
    (out, total, total > cap)
}

/// Pastes text into `state.input`, enforcing size caps and respecting vim mode.
fn paste_into_prompt(state: &mut AppState, text: String, now: Instant) {
    // Drag-and-drop of an image file arrives as a bracketed paste of its path:
    // it goes through the same `[Image N]` paste-token path as a clipboard image.
    if let Some(path) = image_path_from_paste(&text) {
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() <= IMAGE_PASTE_MAX_BYTES as u64 => match std::fs::read(&path) {
                Ok(bytes) => {
                    handle_paste_image_bytes(state, bytes, now);
                    return;
                }
                Err(err) => log::warn!("dropped image {} not read: {err}", path.display()),
            },
            Ok(meta) => log::warn!(
                "dropped image {} too large ({} bytes); pasting the path as text",
                path.display(),
                meta.len()
            ),
            Err(err) => log::warn!("dropped image {} not read: {err}", path.display()),
        }
        // Fall through: paste the path as text.
    }
    let (paste_content, total_chars, truncated) = normalize_paste_prefix(&text, PASTE_MAX_CHARS);
    if truncated {
        log::warn!("pasted text truncated from {total_chars} to {PASTE_MAX_CHARS} chars");
        state.push_toast(
            ToastLevel::Warn,
            format!("Pasted text truncated to {PASTE_MAX_CHARS} chars"),
            Some(format!("Original was {total_chars} chars")),
            now,
        );
    }

    if state.config.ui.vim && state.focus == Focus::Normal {
        apply_vim_paste(
            &mut state.session.vim,
            &mut state.session.input,
            &paste_content,
        );
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

    match read_image_data(runner, platform) {
        ClipboardImageResult::Image(bytes) => handle_paste_image_bytes(state, bytes, now),
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
            vec![]
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
            vec![]
        }
    }
}

fn handle_paste_image_bytes(state: &mut AppState, bytes: Vec<u8>, now: Instant) -> Vec<Action> {
    if bytes.len() > IMAGE_PASTE_MAX_BYTES {
        state.push_toast(
            ToastLevel::Warn,
            "Pasted image is too large".to_string(),
            Some(format!(
                "Maximum image size is {} bytes",
                IMAGE_PASTE_MAX_BYTES
            )),
            now,
        );
        return vec![];
    }
    match state.pasted_images.create_image_file(&bytes) {
        Ok(record) => {
            let token = format!("{} ", record.token_label());
            if state.config.ui.vim && state.focus == Focus::Normal {
                apply_vim_paste(&mut state.session.vim, &mut state.session.input, &token);
            } else {
                state.input.insert_str(&token);
            }
            state.push_toast_linked(
                ToastLevel::Info,
                format!("Image pasted as {}", record.token_label()),
                Some(
                    "Sent as an image attachment with the prompt; remove the token to discard."
                        .to_string(),
                ),
                now,
                record.token_label(),
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
    }
    vec![]
}

/// Cleans up any unreferenced pasted images from disk.
pub fn cleanup_unreferenced_images(state: &mut AppState) {
    state.cleanup_pasted_images();
}

/// File extensions a dropped path is pasted as an image for, not as text.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

/// Whether `path` names an image by its extension.
fn is_image_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A bracketed paste that is exactly one existing image file path (what terminals
/// produce on drag-and-drop): plain, quoted, backslash-escaped or a `file://` URL.
fn image_path_from_paste(text: &str) -> Option<std::path::PathBuf> {
    let t = text.trim();
    if t.is_empty() || t.len() > 4096 || t.contains('\n') || t.contains('\r') {
        return None;
    }
    let t = t.trim_matches(|c| c == '\'' || c == '"');
    let decoded = match t.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None => unescape_spaces(t),
    };
    if !is_image_path(&decoded) {
        return None;
    }
    let path = std::path::PathBuf::from(decoded);
    path.is_file().then_some(path)
}

fn unescape_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.peek().copied()
            && (next == ' ' || next == '\\' || next == '(' || next == ')' || next == '\'')
        {
            out.push(next);
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
