//! Allowlisted readers that write or run code through their own arguments.
//!
//! `sort -o`, `uniq IN OUT`, `tree -o`, `date -s`, `file -C`, `fd -x`, `rg --pre`,
//! `sed 'w f'` / `s///e`, `awk '{print > f}'` / `print | "cmd"` all write a file
//! or run a program while reading like a filter. Short options are read as
//! getopt does (a bundle stops at the first option that takes a value), long
//! ones by the unique prefix GNU tools accept (`sort --out=f`).

use crate::tokenize::Token;

/// Whether an allowlisted `program` writes or executes through `words`.
pub(crate) fn writes_through_own_arguments(program: &str, words: &[Token]) -> bool {
    let args: Vec<&str> = words.iter().skip(1).map(|t| t.text.as_str()).collect();
    match program {
        "fd" => fd_executes(&args),
        // `rg --pre CMD` pipes every file through CMD (`--pre-glob` only filters).
        "rg" => args
            .iter()
            .any(|a| *a == "--pre" || a.starts_with("--pre=")),
        "sort" => {
            has_short(&args, "kotTS", 'o')
                || has_long(&args, "output", 1)
                || has_long(&args, "compress-program", 2)
        }
        "uniq" => uniq_has_output(&args),
        // `-o FILE` writes the listing; `-R` writes `00Tree.html` per directory.
        "tree" => args
            .iter()
            .any(|a| is_short_bundle(a) && a.contains(['o', 'R'])),
        "date" => date_sets_clock(&args),
        // `file -C` compiles a magic file next to it.
        "file" => has_short(&args, "efFmP", 'C') || has_long(&args, "compile", 2),
        // A bare `printenv` dumps every variable, keys included.
        "printenv" => args.is_empty(),
        // `xxd -r` converts a hex dump back to binary; a second operand is the
        // output file. Plain `xxd file` prints a hex dump to stdout.
        "xxd" => xxd_writes(&args),
        _ => false,
    }
}

/// `xxd`: `-r`/`-revert` reverses (writes binary), and a second non-flag
/// operand is the output file. Short flags that take a value (`-c`, `-g`,
/// `-s`, `-l`, `-o`, `-b` has none) consume the next word.
fn xxd_writes(args: &[&str]) -> bool {
    let mut operands = 0;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if *arg == "-r" || *arg == "-revert" {
            return true;
        }
        if is_short_bundle(arg) {
            if arg.contains('r') {
                return true;
            }
            // `-c N`, `-g N`, `-s N`, `-l N`, `-o N`: value in the next word
            // when the flag ends the bundle.
            if arg
                .chars()
                .last()
                .is_some_and(|c| matches!(c, 'c' | 'g' | 's' | 'l' | 'o'))
            {
                index += 1;
            }
            continue;
        }
        if arg.starts_with("--") {
            continue;
        }
        operands += 1;
    }
    operands >= 2
}

fn is_short_bundle(arg: &str) -> bool {
    arg.len() > 1 && arg.starts_with('-') && !arg.starts_with("--")
}

/// Whether `option` appears in a short bundle, reading bundles as getopt does
/// with `values` being the options that take an argument.
fn has_short(args: &[&str], values: &str, option: char) -> bool {
    let mut skip_next = false;
    for arg in args {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        if *arg == "--" {
            return false;
        }
        if !is_short_bundle(arg) {
            continue;
        }
        for (offset, flag) in arg[1..].char_indices() {
            if flag == option {
                return true;
            }
            if values.contains(flag) {
                skip_next = offset + flag.len_utf8() == arg.len() - 1;
                break;
            }
        }
    }
    false
}

/// Whether a `--name[=value]` abbreviating `long` (at least `min` chars) appears.
fn has_long(args: &[&str], long: &str, min: usize) -> bool {
    args.iter().any(|arg| {
        arg.strip_prefix("--")
            .map(|rest| rest.split('=').next().unwrap_or(rest))
            .is_some_and(|name| name.len() >= min && long.starts_with(name))
    })
}

/// `fd -x/-X/--exec/--exec-batch` runs a command per match.
fn fd_executes(args: &[&str]) -> bool {
    has_short(args, "etEdjSco", 'x')
        || has_short(args, "etEdjSco", 'X')
        || args.iter().any(|a| a.starts_with("--exec"))
}

/// `uniq [OPTION]... [INPUT [OUTPUT]]`: a second operand is written.
fn uniq_has_output(args: &[&str]) -> bool {
    let mut operands = 0;
    let mut index = 0;
    let mut options_done = false;
    while let Some(arg) = args.get(index) {
        index += 1;
        if options_done || !arg.starts_with('-') || *arg == "-" {
            operands += 1;
            continue;
        }
        if *arg == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let takes_value = ["skip-fields", "skip-chars", "check-chars"].contains(&long);
            if takes_value {
                index += 1;
            }
            continue;
        }
        let bundle = &arg[1..];
        if let Some(at) = bundle.find(['f', 's', 'w'])
            && at + 1 == bundle.len()
        {
            index += 1;
        }
    }
    operands >= 2
}

/// `date -s STR`, `--set=STR`, or a `MMDDhhmm[[CC]YY][.ss]` operand sets the
/// clock; a `+FORMAT` operand only prints.
fn date_sets_clock(args: &[&str]) -> bool {
    if has_short(args, "dfr", 's') || has_long(args, "set", 1) {
        return true;
    }
    let mut skip_next = false;
    for arg in args {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        if is_short_bundle(arg) {
            skip_next = arg.len() == 2 && matches!(&arg[1..], "d" | "f" | "r");
            continue;
        }
        if arg.starts_with("--") {
            skip_next = !arg.contains('=')
                && ["date", "file", "reference"]
                    .iter()
                    .any(|name| arg[2..] == **name);
            continue;
        }
        if !arg.starts_with('+') {
            return true;
        }
    }
    false
}

/// `sed`: in-place editing, a script from a file, or a script that writes
/// (`w`, `W`, `s///w`) or executes (`e`, `s///e`).
pub(crate) fn sed_writes(args: &[&str]) -> bool {
    let mut scripts = Vec::new();
    let mut operands = Vec::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        if arg.starts_with("-i") || arg.starts_with("--in-place") {
            return true;
        }
        if matches!(*arg, "-f" | "--file") || arg.starts_with("--file=") {
            return true;
        }
        if matches!(*arg, "-e" | "--expression") {
            scripts.extend(args.get(index));
            index += 1;
        } else if let Some(script) = arg.strip_prefix("--expression=") {
            scripts.push(script);
        } else if *arg == "--line-length" {
            index += 1;
        } else if is_short_bundle(arg) {
            let bundle = &arg[1..];
            if bundle.contains(['i', 'f']) {
                return true;
            }
            if bundle.ends_with('l') && !bundle.contains('e') {
                // `-l N`: the line length, not the script.
                index += 1;
            }
            if let Some(at) = bundle.find('e') {
                let attached = &bundle[at + 1..];
                if attached.is_empty() {
                    scripts.extend(args.get(index));
                    index += 1;
                } else {
                    scripts.push(attached);
                }
            }
        } else if !arg.starts_with("--") {
            operands.push(*arg);
        }
    }
    if scripts.is_empty() {
        scripts.extend(operands.first());
    }
    scripts.iter().any(|script| sed_script_writes(script))
}

/// Walk a sed script command by command (addresses skipped) looking for
/// `w`/`W`/`e` and the `w`/`e` flags of `s`.
fn sed_script_writes(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    let skip_delimited = |mut i: usize, delim: char| -> usize {
        while let Some(&c) = chars.get(i) {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == delim {
                return i + 1;
            }
            i += 1;
        }
        i
    };
    let to_line_end = |mut i: usize| -> usize {
        while chars.get(i).is_some_and(|&c| c != '\n') {
            i += 1;
        }
        i
    };
    while let Some(&c) = chars.get(i) {
        match c {
            c if c.is_whitespace() || matches!(c, ';' | '{' | '}' | '!') => i += 1,
            // Addresses: line numbers, `$`, ranges, steps, regexes.
            '0'..='9' | '$' | ',' | '~' | '+' => i += 1,
            '/' => i = skip_delimited(i + 1, '/'),
            '\\' => match chars.get(i + 1) {
                Some(&delim) => i = skip_delimited(i + 2, delim),
                None => i += 1,
            },
            '#' => i = to_line_end(i),
            'w' | 'W' | 'e' => return true,
            's' | 'y' => {
                let Some(&delim) = chars.get(i + 1) else {
                    return false;
                };
                i = skip_delimited(i + 2, delim);
                i = skip_delimited(i, delim);
                if c == 's' {
                    while let Some(&flag) = chars.get(i) {
                        if matches!(flag, 'w' | 'e') {
                            return true;
                        }
                        if !(flag.is_ascii_alphanumeric()) {
                            break;
                        }
                        i += 1;
                    }
                }
            }
            // Text and file-name arguments run to the end of the line.
            'a' | 'i' | 'c' | 'r' | 'R' => i = to_line_end(i),
            'b' | 't' | 'T' | ':' => {
                while chars.get(i).is_some_and(|&c| c != ';' && c != '\n') {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    false
}

/// `awk`: options other than `-F`/`-v` (program files, `--exec`, profiling
/// and dump files, extensions), or a program that pipes (`print | "cmd"`,
/// `"cmd" | getline`), redirects `print`/`printf` output, calls `system`,
/// reads with `getline`, or uses a gawk `@` directive.
pub(crate) fn awk_writes(args: &[&str]) -> bool {
    awk_program(args).is_none_or(awk_program_writes)
}

/// The awk program string of a command (its first non-flag operand). `None`
/// when it is not visible here — read from a `-f` file, after an unknown
/// option, or absent — which both callers treat as "assume it writes / shells
/// out".
fn awk_program<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        match *arg {
            "-F" | "-v" | "--field-separator" | "--assign" => index += 1,
            "--" => return args.get(index).copied(),
            a if a.starts_with("-F")
                || a.starts_with("-v")
                || a.starts_with("--field-separator=")
                || a.starts_with("--assign=") => {}
            a if a.starts_with('-') && a.len() > 1 => return None,
            a => return Some(a),
        }
    }
    None
}

/// Whether an awk program line runs an external command — a pipe to/from a
/// command, `system()`, `getline` from a command, or a gawk `@` directive.
/// Used by the gate to treat such a program as inline interpreter code that
/// shells out, separately from a plain `print > file` write.
pub(crate) fn awk_program_shells_out(program: &str) -> bool {
    let code = awk_code_only(program);
    code.contains(['|', '@']) || code.contains("system") || code.contains("getline")
}

/// Whether the awk program of a command (its first non-flag operand) shells
/// out. `None` program (reads from `-f` files or stdin) counts as shelling out,
/// since its code is not visible here.
pub(crate) fn awk_shells_out(args: &[&str]) -> bool {
    awk_program(args).is_none_or(awk_program_shells_out)
}

fn awk_program_writes(program: &str) -> bool {
    let code = awk_code_only(program);
    if code.contains(['|', '@']) || code.contains("system") || code.contains("getline") {
        return true;
    }
    let mut from = 0;
    while let Some(found) = code[from..].find("print") {
        let mut depth = 0i32;
        for c in code[from + found..].chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ';' | '}' | '\n' => break,
                '>' if depth <= 0 => return true,
                _ => {}
            }
        }
        from += found + "print".len();
    }
    false
}

/// The program with string and regex literals blanked, so a `|` or `>`
/// inside `"a|b"` or `/x>y/` is not read as code.
fn awk_code_only(program: &str) -> String {
    let mut out = String::with_capacity(program.len());
    let mut chars = program.chars().peekable();
    let mut last_significant = None;
    while let Some(c) = chars.next() {
        let starts_regex =
            c == '/' && last_significant.is_none_or(|p: char| "(,{;!~&|=:?\n}".contains(p));
        if c == '"' || starts_regex {
            while let Some(inner) = chars.next() {
                if inner == '\\' {
                    chars.next();
                } else if inner == c {
                    break;
                }
            }
            out.push_str("\"\"");
            last_significant = Some('"');
            continue;
        }
        if !c.is_whitespace() || c == '\n' {
            last_significant = Some(c);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[path = "writers_tests.rs"]
mod writers_tests;
