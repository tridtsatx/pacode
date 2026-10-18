//! Tolerant extraction of string fields from a PARTIAL tool-call JSON argument
//! string (what has streamed in so far), used to scan write/edit content before the
//! call is complete.

/// One decoded string value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrField {
    pub key: String,
    pub value: String,
    /// `false` when the closing quote has not arrived yet (value is a prefix).
    pub complete: bool,
}

/// Decode the JSON string starting at the opening quote `start`. Returns the
/// decoded text, the index after the closing quote (or `json.len()`), and whether
/// the string was closed.
fn read_string(json: &str, start: usize) -> (String, usize, bool) {
    let bytes = json.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return (lossy(out), i + 1, true),
            b'\\' => {
                let Some(&esc) = bytes.get(i + 1) else { break };
                match esc {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'u' => {
                        let Some((ch, used)) = read_unicode(json, i + 2) else {
                            break;
                        };
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        i += 2 + used;
                        continue;
                    }
                    other => out.push(other),
                }
                i += 2;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    (lossy(out), bytes.len(), false)
}

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Parse `XXXX` (and a following `\uYYYY` low surrogate) at `at`; returns the char
/// and the number of source bytes consumed after the `\u`.
fn read_unicode(json: &str, at: usize) -> Option<(char, usize)> {
    let hex = json.get(at..at + 4)?;
    let code = u32::from_str_radix(hex, 16).ok()?;
    if (0xD800..0xDC00).contains(&code) {
        let low_src = json.get(at + 4..at + 10)?;
        let low_hex = low_src.strip_prefix("\\u")?;
        let low = u32::from_str_radix(low_hex, 16).ok()?;
        if (0xDC00..0xE000).contains(&low) {
            let c = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
            return Some((char::from_u32(c)?, 10));
        }
        return Some(('\u{FFFD}', 4));
    }
    Some((char::from_u32(code).unwrap_or('\u{FFFD}'), 4))
}

/// All string values whose key is in `wanted`, in document order, at any nesting
/// depth (so `multi_edit`'s `edits[].new_string` are all found). A trailing
/// unterminated value is returned with `complete = false`.
pub fn extract_string_fields(json: &str, wanted: &[&str]) -> Vec<StrField> {
    let bytes = json.as_bytes();
    let mut out = Vec::new();
    let mut last_key: Option<String> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let (s, end, complete) = read_string(json, i);
                if !complete {
                    if let Some(key) = last_key.take()
                        && wanted.contains(&key.as_str())
                    {
                        out.push(StrField {
                            key,
                            value: s,
                            complete: false,
                        });
                    }
                    break;
                }
                let mut j = end;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b':') {
                    last_key = Some(s);
                    i = j + 1;
                } else {
                    if let Some(key) = last_key.take()
                        && wanted.contains(&key.as_str())
                    {
                        out.push(StrField {
                            key,
                            value: s,
                            complete: true,
                        });
                    }
                    i = end;
                }
            }
            b if b.is_ascii_whitespace() => i += 1,
            _ => {
                last_key = None;
                i += 1;
            }
        }
    }
    out
}
