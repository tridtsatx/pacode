//! Shell tokenizer: words, quotes, escapes, and control operators.
//!
//! Tuned for risk classification: fails loud rather than quiet so ambiguous
//! segments escalate to confirmation.

/// One shell word or operator with classification metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    /// True when this segment's stdin comes from a pipe.
    pub receives_pipe: bool,
    /// True for `>` / `>|` redirect destinations, which are truncated on open.
    pub is_truncating_redirect_target: bool,
    /// True for control operators like `&&`, `|`, `2>&1`, which are never path targets.
    pub is_operator: bool,
}

impl Token {
    pub fn word(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            receives_pipe: false,
            is_truncating_redirect_target: false,
            is_operator: false,
        }
    }

    pub fn operator(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            receives_pipe: false,
            is_truncating_redirect_target: false,
            is_operator: true,
        }
    }

    /// The command name without its directory (`/bin/rm` -> `rm`).
    pub fn basename(&self) -> String {
        self.text
            .rsplit('/')
            .next()
            .unwrap_or(&self.text)
            .to_string()
    }

    pub fn is_flag(&self) -> bool {
        self.text.starts_with('-') && self.text.len() > 1
    }

    /// Whether this flag requests recursion, including bundles like `-rf` or `-Rf`.
    pub fn is_recursive_flag(&self) -> bool {
        if !self.is_flag() {
            return false;
        }
        if self.text.starts_with("--") {
            return self.text == "--recursive";
        }
        self.text.contains('r') || self.text.contains('R')
    }
}

/// Operators that separate one simple command from the next.
const SEGMENT_SEPARATORS: &[&str] = &["&&", "||", ";", "|", "\n", "(", ")", "$(", "`"];

/// Split a command line into individual simple command segments.
pub fn split_segments(command: &str) -> Vec<Vec<Token>> {
    let tokens = tokenize(command);
    let mut segments = Vec::new();
    let mut current = Vec::new();

    let mut next_receives_pipe = false;
    for token in tokens {
        if token.is_operator && SEGMENT_SEPARATORS.contains(&token.text.as_str()) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            next_receives_pipe = token.text == "|";
            continue;
        }
        let mut token = token;
        token.receives_pipe = next_receives_pipe;
        next_receives_pipe = false;
        current.push(token);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Tokenize a shell command line, resolving quotes and escapes.
pub fn tokenize(command: &str) -> Vec<Token> {
    let command = without_heredoc_bodies(command);
    let mut tokens: Vec<Token> = Vec::new();
    let mut current = String::new();
    let mut has_content = false;
    let mut chars = command.chars().peekable();
    let mut pending_redirect = false;

    macro_rules! flush {
        () => {
            if has_content {
                let mut token = Token::word(std::mem::take(&mut current));
                #[allow(unused_assignments)]
                {
                    if pending_redirect {
                        token.is_truncating_redirect_target = true;
                        pending_redirect = false;
                    }
                    has_content = false;
                }
                tokens.push(token);
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has_content = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    current.push(q);
                }
            }
            '"' => {
                has_content = true;
                while let Some(q) = chars.next() {
                    if q == '"' {
                        break;
                    }
                    if q == '\\'
                        && let Some(&next) = chars.peek()
                        && matches!(next, '"' | '\\' | '$' | '`')
                    {
                        current.push(next);
                        chars.next();
                        continue;
                    }
                    current.push(q);
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    has_content = true;
                    current.push(next);
                }
            }
            ' ' | '\t' => flush!(),
            '\n' | ';' => {
                flush!();
                tokens.push(Token::operator(c.to_string()));
            }
            '&' => {
                flush!();
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::operator("&&"));
                } else if chars.peek() == Some(&'>') {
                    chars.next();
                    pending_redirect = true;
                    tokens.push(Token::operator("&>"));
                } else {
                    tokens.push(Token::operator("&"));
                }
            }
            '|' => {
                flush!();
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::operator("||"));
                } else {
                    tokens.push(Token::operator("|"));
                }
            }
            '(' => {
                flush!();
                tokens.push(Token::operator("("));
            }
            ')' => {
                flush!();
                tokens.push(Token::operator(")"));
            }
            '`' => {
                flush!();
                tokens.push(Token::operator("`"));
            }
            '$' => {
                if chars.peek() == Some(&'(') {
                    flush!();
                    chars.next();
                    tokens.push(Token::operator("$("));
                } else {
                    has_content = true;
                    current.push('$');
                }
            }
            '>' => {
                // Check if current is fd prefix like "2" or "1"
                let is_fd_prefix = has_content && current.chars().all(|d| d.is_ascii_digit());
                if is_fd_prefix {
                    let fd = std::mem::take(&mut current);
                    has_content = false;
                    // Check for fd duplication: >&1 or >&2 or >&-
                    if chars.peek() == Some(&'&') {
                        let mut lookahead = chars.clone();
                        lookahead.next(); // skip '&'
                        if let Some(target) = lookahead.next()
                            && (target.is_ascii_digit() || target == '-')
                        {
                            chars.next(); // consume '&'
                            chars.next(); // consume target
                            tokens.push(Token::operator(format!("{fd}>&{target}")));
                            continue;
                        }
                    }
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::operator(format!("{fd}>>")));
                        continue;
                    }
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    pending_redirect = true;
                    tokens.push(Token::operator(format!("{fd}>")));
                } else {
                    flush!();
                    if chars.peek() == Some(&'&') {
                        let mut lookahead = chars.clone();
                        lookahead.next();
                        if let Some(target) = lookahead.next()
                            && (target.is_ascii_digit() || target == '-')
                        {
                            chars.next();
                            chars.next();
                            tokens.push(Token::operator(format!(">&{target}")));
                            continue;
                        }
                    }
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        tokens.push(Token::operator(">>"));
                    } else {
                        if chars.peek() == Some(&'|') {
                            chars.next();
                        }
                        pending_redirect = true;
                        tokens.push(Token::operator(">"));
                    }
                }
            }
            '<' => {
                flush!();
                if chars.peek() == Some(&'<') {
                    chars.next();
                    if chars.peek() == Some(&'<') {
                        chars.next();
                        tokens.push(Token::operator("<<<"));
                    } else {
                        tokens.push(Token::operator("<<"));
                    }
                } else {
                    tokens.push(Token::operator("<"));
                }
            }
            _ => {
                has_content = true;
                current.push(c);
            }
        }
    }
    flush!();

    tokens
}

/// Strip heredoc payload lines before tokenizing.
fn without_heredoc_bodies(command: &str) -> String {
    let lines: Vec<&str> = command.split_inclusive('\n').collect();
    let mut output = String::with_capacity(command.len());
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        output.push_str(line);
        let delimiters = heredoc_delimiters(line.trim_end_matches('\n'));
        index += 1;

        for (delimiter, strip_tabs) in delimiters {
            while index < lines.len() {
                let candidate = lines[index].trim_end_matches(['\r', '\n']);
                let candidate = if strip_tabs {
                    candidate.trim_start_matches('\t')
                } else {
                    candidate
                };
                index += 1;
                if candidate == delimiter {
                    output.push('\n');
                    break;
                }
            }
        }
    }

    output
}

fn heredoc_delimiters(line: &str) -> Vec<(String, bool)> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    let mut quote = None;

    while index + 1 < bytes.len() {
        let byte = bytes[index];
        if let Some(end) = quote {
            if byte == b'\\' && end == b'"' {
                index += 2;
                continue;
            }
            if byte == end {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b'\\' {
            index += 2;
            continue;
        }
        if byte != b'<' || bytes[index + 1] != b'<' {
            index += 1;
            continue;
        }

        index += 2;
        if bytes.get(index) == Some(&b'<') {
            index += 1;
            continue;
        }
        let strip_tabs = bytes.get(index) == Some(&b'-');
        if strip_tabs {
            index += 1;
        }
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }

        let mut delimiter = String::new();
        let mut delimiter_quote = None;
        while index < bytes.len() {
            let byte = bytes[index];
            if let Some(end) = delimiter_quote {
                if byte == end {
                    delimiter_quote = None;
                } else if byte == b'\\' && end == b'"' && index + 1 < bytes.len() {
                    index += 1;
                    delimiter.push(bytes[index] as char);
                } else {
                    delimiter.push(byte as char);
                }
            } else if matches!(byte, b'\'' | b'"') {
                delimiter_quote = Some(byte);
            } else if byte == b'\\' && index + 1 < bytes.len() {
                index += 1;
                delimiter.push(bytes[index] as char);
            } else if byte.is_ascii_whitespace() || matches!(byte, b';' | b'&' | b'|') {
                break;
            } else {
                delimiter.push(byte as char);
            }
            index += 1;
        }
        if !delimiter.is_empty() {
            found.push((delimiter, strip_tabs));
        }
    }

    found
}

#[cfg(test)]
#[path = "tokenize_tests.rs"]
mod tokenize_tests;
