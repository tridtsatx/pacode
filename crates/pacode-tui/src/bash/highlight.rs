//! Hand-written tokenizer for syntax highlighting in bash mode (prompt-input feature 1).
//!
//! Spans carry semantic roles (`pacode_render::BashRole`) so they can be rendered
//! with palette colors and tested without a terminal.
//! Concatenating span texts reconstructs the input string byte-for-byte.

use pacode_render::BashRole;

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod highlight_tests;

/// A slice of the input string tagged with a semantic bash role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashSpan<'a> {
    pub text: &'a str,
    pub role: BashRole,
}

/// Tokenize an input line into highlighted spans.
pub fn tokenize(line: &str) -> Vec<BashSpan<'_>> {
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut expecting_command = true;

    while i < len {
        let ch = line[i..].chars().next().unwrap_or(' ');

        // 1. Whitespace
        if ch.is_whitespace() {
            let start = i;
            while i < len {
                let c = line[i..].chars().next().unwrap_or(' ');
                if !c.is_whitespace() {
                    break;
                }
                i += c.len_utf8();
            }
            spans.push(BashSpan {
                text: &line[start..i],
                role: BashRole::Argument,
            });
            continue;
        }

        // 2. Operators: multi-char check first
        if line[i..].starts_with("2>&1") {
            spans.push(BashSpan {
                text: &line[i..i + 4],
                role: BashRole::Operator,
            });
            i += 4;
            continue;
        }
        if line[i..].starts_with("&&") || line[i..].starts_with("||") || line[i..].starts_with(">>")
        {
            let op = &line[i..i + 2];
            let resets_cmd = op == "&&" || op == "||";
            spans.push(BashSpan {
                text: op,
                role: BashRole::Operator,
            });
            if resets_cmd {
                expecting_command = true;
            }
            i += 2;
            continue;
        }
        if matches!(ch, '|' | ';' | '>' | '<' | '&') {
            let resets_cmd = ch == '|' || ch == ';';
            spans.push(BashSpan {
                text: &line[i..i + 1],
                role: BashRole::Operator,
            });
            if resets_cmd {
                expecting_command = true;
            }
            i += 1;
            continue;
        }

        // 3. Quoted strings (single and double quotes, including unterminated trailing quote)
        if ch == '\'' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\'' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(BashSpan {
                text: &line[start..i],
                role: BashRole::String,
            });
            expecting_command = false;
            continue;
        }

        if ch == '"' {
            let start = i;
            i += 1;
            let mut escaped = false;
            while i < len {
                if escaped {
                    escaped = false;
                    i += 1;
                } else if bytes[i] == b'\\' {
                    escaped = true;
                    i += 1;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            spans.push(BashSpan {
                text: &line[start..i],
                role: BashRole::String,
            });
            expecting_command = false;
            continue;
        }

        // 4. Variables ($NAME or ${NAME})
        if ch == '$' && i + 1 < len {
            let next_ch = line[i + 1..].chars().next().unwrap_or(' ');
            if next_ch == '{' {
                let start = i;
                i += 2;
                while i < len {
                    if bytes[i] == b'}' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                spans.push(BashSpan {
                    text: &line[start..i],
                    role: BashRole::Variable,
                });
                expecting_command = false;
                continue;
            } else if matches!(next_ch, '?' | '!' | '*' | '@' | '#' | '$') {
                let start = i;
                i += 1 + next_ch.len_utf8();
                spans.push(BashSpan {
                    text: &line[start..i],
                    role: BashRole::Variable,
                });
                expecting_command = false;
                continue;
            } else if next_ch.is_alphanumeric() || next_ch == '_' {
                let start = i;
                i += 1;
                while i < len {
                    let c = line[i..].chars().next().unwrap_or(' ');
                    if !c.is_alphanumeric() && c != '_' {
                        break;
                    }
                    i += c.len_utf8();
                }
                spans.push(BashSpan {
                    text: &line[start..i],
                    role: BashRole::Variable,
                });
                expecting_command = false;
                continue;
            }
        }

        // 5. Option flags (-x, --long, -rf)
        if ch == '-' {
            let start = i;
            while i < len {
                let c = line[i..].chars().next().unwrap_or(' ');
                if c.is_whitespace() || matches!(c, '|' | ';' | '>' | '<' | '&' | '"' | '\'') {
                    break;
                }
                i += c.len_utf8();
            }
            spans.push(BashSpan {
                text: &line[start..i],
                role: BashRole::Flag,
            });
            expecting_command = false;
            continue;
        }

        // 6. Regular word: Command or Argument
        let start = i;
        while i < len {
            let c = line[i..].chars().next().unwrap_or(' ');
            if c.is_whitespace() || matches!(c, '|' | ';' | '>' | '<' | '&' | '"' | '\'' | '$') {
                break;
            }
            i += c.len_utf8();
        }

        let role = if expecting_command {
            expecting_command = false;
            BashRole::Command
        } else {
            BashRole::Argument
        };

        spans.push(BashSpan {
            text: &line[start..i],
            role,
        });
    }

    spans
}
