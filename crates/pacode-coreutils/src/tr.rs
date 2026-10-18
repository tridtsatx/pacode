//! `tr [-c] [-d] [-s] SET1 [SET2]` over stdin.

use std::path::Path;

use crate::args::{Flag, parse, sw};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "tr_tests.rs"]
mod tr_tests;

enum Item {
    /// A plain character (may start a range).
    Ch(char),
    /// An escaped character (never a range operator).
    Lit(char),
    Class(Vec<char>),
}

fn class_chars(name: &str) -> Option<Vec<char>> {
    let range = |a: char, b: char| (a..=b).collect::<Vec<char>>();
    Some(match name {
        "upper" => range('A', 'Z'),
        "lower" => range('a', 'z'),
        "digit" => range('0', '9'),
        "alpha" => [range('A', 'Z'), range('a', 'z')].concat(),
        "alnum" => [range('0', '9'), range('A', 'Z'), range('a', 'z')].concat(),
        "xdigit" => [range('0', '9'), range('A', 'F'), range('a', 'f')].concat(),
        "space" => vec![' ', '\t', '\n', '\r', '\u{b}', '\u{c}'],
        "blank" => vec![' ', '\t'],
        "punct" => (0x21u8..=0x7e)
            .map(char::from)
            .filter(|c| c.is_ascii_punctuation())
            .collect(),
        _ => return None,
    })
}

fn expand_set(text: &str) -> Result<Vec<char>, UtilError> {
    let chars: Vec<char> = text.chars().collect();
    let mut items: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '[' && chars.get(i + 1) == Some(&':') {
            let rest: String = chars[i + 2..].iter().collect();
            if let Some(end) = rest.find(":]") {
                let name = &rest[..end];
                let class = class_chars(name)
                    .ok_or_else(|| UtilError::usage(format!("unknown character class '{name}'")))?;
                items.push(Item::Class(class));
                i += 2 + name.chars().count() + 2;
                continue;
            }
        }
        if c == '\\' {
            let Some(&next) = chars.get(i + 1) else {
                return Err(UtilError::usage("trailing backslash in set"));
            };
            let lit = match next {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '0' => '\0',
                other => other,
            };
            items.push(Item::Lit(lit));
            i += 2;
            continue;
        }
        items.push(Item::Ch(c));
        i += 1;
    }
    let single = |item: &Item| match item {
        Item::Ch(c) | Item::Lit(c) => Some(*c),
        Item::Class(_) => None,
    };
    let mut out = Vec::new();
    let mut k = 0;
    while k < items.len() {
        if k + 2 < items.len()
            && matches!(items[k + 1], Item::Ch('-'))
            && let (Some(lo), Some(hi)) = (single(&items[k]), single(&items[k + 2]))
        {
            if lo > hi {
                return Err(UtilError::usage(format!(
                    "range '{lo}-{hi}' is in reverse order"
                )));
            }
            out.extend(lo..=hi);
            k += 3;
            continue;
        }
        match &items[k] {
            Item::Ch(c) | Item::Lit(c) => out.push(*c),
            Item::Class(cs) => out.extend(cs.iter().copied()),
        }
        k += 1;
    }
    Ok(out)
}

/// `tr [-c] [-d] [-s] SET1 [SET2]`: translate, delete or squeeze characters of stdin.
pub fn tr(args: &[String], stdin: Option<&[u8]>, _cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("c", 'c', "complement"),
        sw("c", 'C', ""),
        sw("d", 'd', "delete"),
        sw("s", 's', "squeeze-repeats"),
    ];
    let p = parse(args, SPEC, false)?;
    let complement = p.has("c");
    let delete = p.has("d");
    let squeeze = p.has("s");
    let sets = &p.operands;
    if sets.is_empty() || sets.len() > 2 {
        return Err(UtilError::usage("expected SET1 and optionally SET2"));
    }
    let Some(data) = stdin else {
        return Err(UtilError::usage("tr reads piped input only"));
    };
    let set1 = expand_set(&sets[0])?;
    let set2 = match sets.get(1) {
        Some(s) => expand_set(s)?,
        None => Vec::new(),
    };
    let translating = !delete && !set2.is_empty();
    if delete && !squeeze && sets.len() == 2 {
        return Err(UtilError::usage(
            "extra operand with -d (SET2 is only used with -s)",
        ));
    }
    if !delete && !squeeze && set2.is_empty() {
        return Err(UtilError::usage("missing SET2 for translation"));
    }
    let in_set1 = |c: char| set1.contains(&c) != complement;
    let squeeze_set: &Vec<char> = if translating || (delete && squeeze) {
        &set2
    } else {
        &set1
    };
    let squeeze_complement = !(translating || (delete && squeeze)) && complement;

    let text = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    for c in text.chars() {
        let mapped = if delete {
            if in_set1(c) {
                continue;
            }
            c
        } else if translating {
            if complement {
                if in_set1(c) {
                    set2.last().copied().unwrap_or(c)
                } else {
                    c
                }
            } else {
                match set1.iter().position(|x| *x == c) {
                    Some(idx) => set2.get(idx).or(set2.last()).copied().unwrap_or(c),
                    None => c,
                }
            }
        } else {
            c
        };
        if squeeze {
            let member = squeeze_set.contains(&mapped) != squeeze_complement;
            if member && prev == Some(mapped) {
                continue;
            }
        }
        prev = Some(mapped);
        out.push(mapped);
    }
    Ok(UtilOutput::ok(out))
}
