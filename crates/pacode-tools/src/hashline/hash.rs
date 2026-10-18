//! Short content hashes for line anchors.

/// Characters in a line hash.
pub const HASH_LEN: usize = 2;

/// 32 symbols without the look-alikes `i l o u`; two of them give 1024 distinct hashes.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// 10-bit hash of a line (FNV-1a, folded). Trailing whitespace (including `\r`) is
/// ignored; leading indentation counts.
pub(crate) fn hash_code(line: &str) -> u16 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in line.trim_end().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= h >> 32;
    h ^= h >> 17;
    u16::try_from(h & 0x3ff).unwrap_or(0)
}

fn symbol(index: u16) -> char {
    ALPHABET
        .get(usize::from(index & 31))
        .map_or('0', |b| char::from(*b))
}

/// Encode a 10-bit hash as [`HASH_LEN`] symbols.
pub(crate) fn encode(code: u16) -> String {
    let mut s = String::with_capacity(HASH_LEN);
    s.push(symbol(code >> 5));
    s.push(symbol(code));
    s
}

fn decode_symbol(c: char) -> Option<u16> {
    let c = c.to_ascii_lowercase();
    let pos = ALPHABET.iter().position(|b| char::from(*b) == c)?;
    u16::try_from(pos).ok()
}

/// Decode a hash string; `None` unless it is exactly [`HASH_LEN`] valid symbols.
pub(crate) fn decode(s: &str) -> Option<u16> {
    let mut chars = s.chars();
    let hi = decode_symbol(chars.next()?)?;
    let lo = decode_symbol(chars.next()?)?;
    if chars.next().is_some() {
        return None;
    }
    Some((hi << 5) | lo)
}

/// Short hash string of one line's content (no newline), e.g. `"k3"`.
pub fn line_hash(line: &str) -> String {
    encode(hash_code(line))
}

/// If `line` looks like a copied display row (`12#ab| text`), return the text part.
pub(crate) fn strip_anchor_prefix(line: &str) -> Option<&str> {
    let (num, rest) = line.split_once('#')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hash, text) = rest.split_once('|')?;
    decode(hash)?;
    Some(text.strip_prefix(' ').unwrap_or(text))
}
