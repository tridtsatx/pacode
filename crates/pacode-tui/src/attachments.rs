//! Pending image attachments of the prompt, shown as chips in the input.
//!
//! A chip is the text `[Image N · 1.2 MB]` inside the prompt, so it moves, wraps and is
//! edited like any other text; backspace/delete next to a chip removes it whole. The
//! prepared bytes live in [`PendingAttachments`] (owner: `AppState`, cap
//! [`MAX_PENDING`] images, dropped when the message is built or the TUI exits). On
//! send, chips present in the text become [`Attachment`]s in order of appearance and
//! the text keeps a plain `[Image K]` marker per image.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use pacode_image::prepare::{MAX_INPUT_BYTES, Prepared, prepare};
use pacode_types::{Attachment, MAX_IMAGES_PER_MESSAGE, format_bytes};

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod attachments_tests;

/// Pending images kept at once (one message can carry at most this many).
pub const MAX_PENDING: usize = MAX_IMAGES_PER_MESSAGE;

/// File extensions attached as images instead of text.
pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

const CHIP_HEAD: [char; 7] = ['[', 'I', 'm', 'a', 'g', 'e', ' '];
const CHIP_SEP: [char; 3] = [' ', '\u{b7}', ' '];

/// One prepared image waiting to be sent.
#[derive(Clone, Debug)]
pub struct PendingImage {
    pub n: u32,
    pub name: Option<String>,
    pub prepared: Prepared,
}

/// Prepared images of the prompt being edited.
#[derive(Debug, Default)]
pub struct PendingAttachments {
    items: Vec<PendingImage>,
    next_n: u32,
}

/// A chip found in prompt text; offsets are char indices, `end` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chip {
    pub start: usize,
    pub end: usize,
    pub n: u32,
}

/// Why an image could not be attached.
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("at most 8 images per message")]
    TooMany,
    #[error("{0}")]
    Image(#[from] pacode_image::PrepareError),
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
}

impl PendingAttachments {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, n: u32) -> Option<&PendingImage> {
        self.items.iter().find(|i| i.n == n)
    }

    /// Prepare `bytes` and keep them; returns the chip text to insert.
    pub fn add(&mut self, bytes: &[u8], name: Option<String>) -> Result<String, AttachError> {
        if self.items.len() >= MAX_PENDING {
            return Err(AttachError::TooMany);
        }
        let prepared = prepare(bytes)?;
        self.next_n += 1;
        let n = self.next_n;
        let chip = chip_text(n, prepared.bytes.len());
        self.items.push(PendingImage { n, name, prepared });
        Ok(chip)
    }

    /// Read and attach an image file.
    pub fn add_file(&mut self, path: &Path) -> Result<String, AttachError> {
        let read_err = |source| AttachError::Read {
            path: path.display().to_string(),
            source,
        };
        let meta = std::fs::metadata(path).map_err(read_err)?;
        if meta.len() > MAX_INPUT_BYTES as u64 {
            return Err(AttachError::Image(
                pacode_image::PrepareError::InputTooLarge {
                    size: meta.len() as usize,
                    limit: MAX_INPUT_BYTES,
                },
            ));
        }
        let bytes = std::fs::read(path).map_err(read_err)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.add(&bytes, name)
    }

    /// Keep only the images numbered in `used` (chips the user deleted are gone).
    pub fn retain_numbers(&mut self, used: &[u32]) {
        self.items.retain(|i| used.contains(&i.n));
        if self.items.is_empty() {
            self.next_n = 0;
        }
    }

    /// Turn prompt `text` into the message text and its attachments: chips of pending
    /// images become `[Image K]` (K = order of first appearance) and the images are
    /// moved out. Chips without a pending image stay as literal text.
    pub fn take_for_text(&mut self, text: &str) -> (String, Vec<Attachment>) {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut order: Vec<u32> = Vec::new();
        let mut attachments = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if let Some((end, n)) = parse_chip(&chars, i)
                && let Some(image) = self.get(n)
            {
                let k = match order.iter().position(|x| *x == n) {
                    Some(pos) => pos + 1,
                    None => {
                        order.push(n);
                        attachments.push(attachment_of(image));
                        order.len()
                    }
                };
                out.push_str(&format!("[Image {k}]"));
                i = end;
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        let used = order;
        self.items.retain(|i| !used.contains(&i.n));
        if self.items.is_empty() {
            self.next_n = 0;
        }
        (out, attachments)
    }
}

fn attachment_of(image: &PendingImage) -> Attachment {
    Attachment {
        mime: image.prepared.mime.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&image.prepared.bytes),
        name: image.name.clone(),
    }
}

/// `[Image 2 · 1.2 MB]`.
pub fn chip_text(n: u32, bytes: usize) -> String {
    format!("[Image {n} \u{b7} {}]", format_bytes(bytes))
}

fn parse_chip(chars: &[char], start: usize) -> Option<(usize, u32)> {
    if chars.get(start..start + CHIP_HEAD.len())? != &CHIP_HEAD[..] {
        return None;
    }
    let digits_start = start + CHIP_HEAD.len();
    let mut i = digits_start;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start || i - digits_start > 6 {
        return None;
    }
    let n: u32 = chars[digits_start..i]
        .iter()
        .collect::<String>()
        .parse()
        .ok()?;
    if chars.get(i..i + CHIP_SEP.len())? != &CHIP_SEP[..] {
        return None;
    }
    i += CHIP_SEP.len();
    let tail_start = i;
    while i < chars.len() && !matches!(chars[i], ']' | '[' | '\n') && i - tail_start <= 16 {
        i += 1;
    }
    if i > tail_start && chars.get(i) == Some(&']') {
        Some((i + 1, n))
    } else {
        None
    }
}

/// All chips in `text`, in order.
pub fn find_chips(text: &str) -> Vec<Chip> {
    let chars: Vec<char> = text.chars().collect();
    let mut chips = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some((end, n)) = parse_chip(&chars, i)
        {
            chips.push(Chip { start: i, end, n });
            i = end;
        } else {
            i += 1;
        }
    }
    chips
}

/// Chip a backspace at `cursor` removes: the cursor sits after or inside it.
pub fn chip_for_backspace(text: &str, cursor: usize) -> Option<Chip> {
    find_chips(text)
        .into_iter()
        .find(|c| c.start < cursor && cursor <= c.end)
}

/// Chip a forward delete at `cursor` removes: the cursor sits before or inside it.
pub fn chip_for_delete(text: &str, cursor: usize) -> Option<Chip> {
    find_chips(text)
        .into_iter()
        .find(|c| c.start <= cursor && cursor < c.end)
}

/// Whether `path` names an image by its extension.
pub fn is_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A bracketed paste that is exactly one existing image file path (what terminals
/// produce on drag-and-drop): plain, quoted, backslash-escaped or a `file://` URL.
pub fn image_path_from_paste(text: &str) -> Option<PathBuf> {
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
    let path = PathBuf::from(decoded);
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
