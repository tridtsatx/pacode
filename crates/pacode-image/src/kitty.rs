//! Kitty graphics protocol encoding and chunking.

use base64::Engine;

#[cfg(test)]
#[path = "kitty_tests.rs"]
mod kitty_tests;

/// Maximum base64 payload bytes allowed per Kitty escape sequence (spec: 4096 bytes).
pub const KITTY_MAX_CHUNK_BYTES: usize = 4096;

/// Chunk a base64-encoded string into Kitty graphics protocol escape sequences.
///
/// First chunk: `\x1b_Ga=T,f=32,s=<w>,v=<h>,c=<cols>,r=<rows>,m=<0|1>;<chunk>\x1b\`
/// Subsequent chunks: `\x1b_Gm=<0|1>;<chunk>\x1b\`
pub fn chunk_kitty_base64(b64: &str, w: u32, h: u32, cols: u16, rows: u16) -> Vec<String> {
    if b64.is_empty() {
        return vec![format!(
            "\x1b_Ga=T,f=32,s={w},v={h},c={cols},r={rows},m=0;\x1b\\"
        )];
    }

    let mut chunks = Vec::new();
    let total_len = b64.len();
    let mut offset = 0;
    let mut first = true;

    while offset < total_len {
        let end = (offset + KITTY_MAX_CHUNK_BYTES).min(total_len);
        let chunk = &b64[offset..end];
        let more = if end < total_len { 1 } else { 0 };

        if first {
            chunks.push(format!(
                "\x1b_Ga=T,f=32,s={w},v={h},c={cols},r={rows},m={more};{chunk}\x1b\\"
            ));
            first = false;
        } else {
            chunks.push(format!("\x1b_Gm={more};{chunk}\x1b\\"));
        }
        offset = end;
    }

    chunks
}

/// Encode raw 32-bit RGBA bytes into a concatenated Kitty graphics protocol escape sequence.
pub fn encode_kitty(rgba: &[u8], w: u32, h: u32, cols: u16, rows: u16) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
    let chunks = chunk_kitty_base64(&b64, w, h, cols, rows);
    chunks.concat()
}
