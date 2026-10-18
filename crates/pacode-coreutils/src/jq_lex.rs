//! Lexer for the supported `jq` subset.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Dot,
    DotDot,
    Field(String),
    Ident(String),
    Num(f64),
    Str(String),
    Var(String),
    LBracket,
    RBracket,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Pipe,
    Comma,
    Colon,
    Semi,
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Alt,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn lex_string(chars: &[char], i: &mut usize) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let Some(&c) = chars.get(*i) else {
            return Err("unterminated string literal".to_string());
        };
        *i += 1;
        match c {
            '"' => return Ok(out),
            '\\' => {
                let Some(&e) = chars.get(*i) else {
                    return Err("unterminated string literal".to_string());
                };
                *i += 1;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = chars.iter().skip(*i).take(4).collect();
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| "invalid \\u escape".to_string())?;
                        *i += 4;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    '(' => return Err("string interpolation \\( ) is not supported".to_string()),
                    other => return Err(format!("invalid escape \\{other}")),
                }
            }
            other => out.push(other),
        }
    }
}

pub(crate) fn lex(source: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = source.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            ' ' | '\t' | '\n' | '\r' => {}
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '.' => {
                if chars.get(i) == Some(&'.') {
                    i += 1;
                    toks.push(Tok::DotDot);
                } else if chars.get(i).is_some_and(|c| is_ident_start(*c)) {
                    let start = i;
                    while i < chars.len() && is_ident_char(chars[i]) {
                        i += 1;
                    }
                    toks.push(Tok::Field(chars[start..i].iter().collect()));
                } else {
                    toks.push(Tok::Dot);
                }
            }
            '"' => toks.push(Tok::Str(lex_string(&chars, &mut i)?)),
            '$' => {
                let start = i;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
                if start == i {
                    return Err("expected a variable name after `$`".to_string());
                }
                toks.push(Tok::Var(chars[start..i].iter().collect()));
            }
            '@' => {
                let start = i - 1;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            c if c.is_ascii_digit() => {
                let start = i - 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                if matches!(chars.get(i), Some('e') | Some('E')) {
                    i += 1;
                    if matches!(chars.get(i), Some('+') | Some('-')) {
                        i += 1;
                    }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                let n = text
                    .parse::<f64>()
                    .map_err(|_| format!("invalid number '{text}'"))?;
                toks.push(Tok::Num(n));
            }
            c if is_ident_start(c) => {
                let start = i - 1;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            '[' => toks.push(Tok::LBracket),
            ']' => toks.push(Tok::RBracket),
            '(' => toks.push(Tok::LParen),
            ')' => toks.push(Tok::RParen),
            '{' => toks.push(Tok::LBrace),
            '}' => toks.push(Tok::RBrace),
            ',' => toks.push(Tok::Comma),
            ':' => toks.push(Tok::Colon),
            ';' => toks.push(Tok::Semi),
            '?' => toks.push(Tok::Question),
            '+' => toks.push(Tok::Plus),
            '-' => toks.push(Tok::Minus),
            '*' => toks.push(Tok::Star),
            '%' => toks.push(Tok::Percent),
            '|' => {
                if chars.get(i) == Some(&'=') {
                    return Err("update assignment `|=` is not supported".to_string());
                }
                toks.push(Tok::Pipe);
            }
            '/' => {
                if chars.get(i) == Some(&'/') {
                    i += 1;
                    toks.push(Tok::Alt);
                } else {
                    toks.push(Tok::Slash);
                }
            }
            '=' => {
                if chars.get(i) == Some(&'=') {
                    i += 1;
                    toks.push(Tok::EqEq);
                } else {
                    return Err("assignment `=` is not supported".to_string());
                }
            }
            '!' => {
                if chars.get(i) == Some(&'=') {
                    i += 1;
                    toks.push(Tok::Ne);
                } else {
                    return Err("unexpected `!`".to_string());
                }
            }
            '<' => {
                if chars.get(i) == Some(&'=') {
                    i += 1;
                    toks.push(Tok::Le);
                } else {
                    toks.push(Tok::Lt);
                }
            }
            '>' => {
                if chars.get(i) == Some(&'=') {
                    i += 1;
                    toks.push(Tok::Ge);
                } else {
                    toks.push(Tok::Gt);
                }
            }
            other => return Err(format!("unexpected character `{other}`")),
        }
    }
    Ok(toks)
}
