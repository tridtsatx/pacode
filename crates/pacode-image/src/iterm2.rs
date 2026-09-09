//! iTerm2 inline image protocol encoding (OSC 1337).

use base64::Engine;

#[cfg(test)]
#[path = "iterm2_tests.rs"]
mod iterm2_tests;

/// Encode raw file bytes into an iTerm2 inline image escape sequence (OSC 1337).
pub fn encode_iterm2(file_bytes: &[u8], cols: u16, rows: u16) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(file_bytes);
    format!("\x1b]1337;File=inline=1;width={cols};height={rows};preserveAspectRatio=1:{b64}\x07")
}
