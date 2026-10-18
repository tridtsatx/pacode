//! `basename`, `dirname`, `realpath`, `stat`, `du`.

use std::fs::Metadata;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::args::{Flag, parse, parse_usize, sw, val};
use crate::io::{io_error, resolve};
use crate::{UtilError, UtilOutput, UtilResult};

#[cfg(test)]
#[path = "fsinfo_tests.rs"]
mod fsinfo_tests;

/// Entries visited by one `du` before giving up.
const DU_ENTRY_LIMIT: usize = 200_000;
const DU_DEPTH_LIMIT: usize = 256;

fn base_of(name: &str) -> String {
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() {
        return if name.is_empty() {
            String::new()
        } else {
            "/".to_string()
        };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

fn dir_of(name: &str) -> String {
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() {
        return if name.is_empty() {
            ".".to_string()
        } else {
            "/".to_string()
        };
    }
    match trimmed.rfind('/') {
        None => ".".to_string(),
        Some(i) => {
            let dir = trimmed[..i].trim_end_matches('/');
            if dir.is_empty() {
                "/".to_string()
            } else {
                dir.to_string()
            }
        }
    }
}

/// `basename NAME [SUFFIX]`, `basename -a NAME...`, `basename -s SUFFIX NAME...`
pub fn basename(args: &[String], _stdin: Option<&[u8]>, _cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[sw("a", 'a', "multiple"), val("s", 's', "suffix")];
    let p = parse(args, SPEC, false)?;
    let multiple = p.has("a") || p.has("s");
    let (names, suffix): (&[String], Option<&str>) = if multiple {
        (p.operands.as_slice(), p.get("s"))
    } else if p.operands.len() == 2 {
        (&p.operands[..1], Some(p.operands[1].as_str()))
    } else {
        (p.operands.as_slice(), None)
    };
    if names.is_empty() || (!multiple && names.len() > 1) {
        return Err(UtilError::usage("expected NAME [SUFFIX]"));
    }
    let mut out = String::new();
    for name in names {
        let mut base = base_of(name);
        if let Some(suffix) = suffix
            && !suffix.is_empty()
            && base != suffix
            && base.ends_with(suffix)
        {
            base.truncate(base.len() - suffix.len());
        }
        out.push_str(&base);
        out.push('\n');
    }
    Ok(UtilOutput::ok(out))
}

/// `dirname NAME...`
pub fn dirname(args: &[String], _stdin: Option<&[u8]>, _cwd: &Path) -> UtilResult {
    let p = parse(args, &[], false)?;
    if p.operands.is_empty() {
        return Err(UtilError::usage("missing operand"));
    }
    let mut out = String::new();
    for name in &p.operands {
        out.push_str(&dir_of(name));
        out.push('\n');
    }
    Ok(UtilOutput::ok(out))
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `realpath [-e] [-m] [-s] PATH...`
pub fn realpath(args: &[String], _stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("e", 'e', "canonicalize-existing"),
        sw("m", 'm', "canonicalize-missing"),
        sw("s", 's', "strip"),
    ];
    let p = parse(args, SPEC, false)?;
    if p.operands.is_empty() {
        return Err(UtilError::usage("missing operand"));
    }
    let mut out = String::new();
    for name in &p.operands {
        let abs = resolve(cwd, name);
        let resolved = if p.has("s") {
            lexical(&abs)
        } else {
            match abs.canonicalize() {
                Ok(path) => path,
                Err(error) if p.has("e") => return Err(io_error(name, &error)),
                Err(_) => {
                    let lex = lexical(&abs);
                    if p.has("m") {
                        lex
                    } else {
                        let parent = lex.parent().unwrap_or(Path::new("/"));
                        let parent = parent.canonicalize().map_err(|e| io_error(name, &e))?;
                        match lex.file_name() {
                            Some(file) => parent.join(file),
                            None => parent,
                        }
                    }
                }
            }
        };
        out.push_str(&resolved.to_string_lossy());
        out.push('\n');
    }
    Ok(UtilOutput::ok(out))
}

// -------------------------------------------------------------- stat

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn format_utc(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(unix)]
fn mode_bits(meta: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_bits(meta: &Metadata) -> u32 {
    if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

fn type_name(meta: &Metadata) -> &'static str {
    let ft = meta.file_type();
    if ft.is_symlink() {
        "symbolic link"
    } else if ft.is_dir() {
        "directory"
    } else if ft.is_file() {
        "regular file"
    } else {
        "special file"
    }
}

fn mode_string(meta: &Metadata) -> String {
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        'l'
    } else if ft.is_dir() {
        'd'
    } else if ft.is_file() {
        '-'
    } else {
        '?'
    };
    let bits = mode_bits(meta);
    let mut s = String::from(kind);
    for shift in [6u32, 3, 0] {
        let triple = (bits >> shift) & 0o7;
        s.push(if triple & 4 != 0 { 'r' } else { '-' });
        s.push(if triple & 2 != 0 { 'w' } else { '-' });
        s.push(if triple & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn mtime_secs(meta: &Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        Err(e) => Some(-(e.duration().as_secs() as i64)),
    }
}

fn stat_format(format: &str, name: &str, meta: &Metadata) -> Result<String, UtilError> {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push_str(name),
            Some('s') => out.push_str(&meta.len().to_string()),
            Some('a') => out.push_str(&format!("{:o}", mode_bits(meta))),
            Some('A') => out.push_str(&mode_string(meta)),
            Some('F') => out.push_str(type_name(meta)),
            Some('Y') => out.push_str(&mtime_secs(meta).unwrap_or(0).to_string()),
            Some('y') => out.push_str(&format_utc(mtime_secs(meta).unwrap_or(0))),
            Some('%') => out.push('%'),
            Some(other) => {
                return Err(UtilError::usage(format!(
                    "unsupported format sequence '%{other}' (use %n %s %a %A %F %Y %y %%)"
                )));
            }
            None => return Err(UtilError::usage("format ends with a lone '%'")),
        }
    }
    Ok(out)
}

/// `stat [-L] [-c FORMAT] FILE...`
pub fn stat(args: &[String], _stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[sw("L", 'L', "dereference"), val("c", 'c', "format")];
    let p = parse(args, SPEC, false)?;
    if p.operands.is_empty() {
        return Err(UtilError::usage("missing operand"));
    }
    let mut out = String::new();
    for name in &p.operands {
        let path = resolve(cwd, name);
        let fetched = if p.has("L") {
            std::fs::metadata(&path)
        } else {
            std::fs::symlink_metadata(&path)
        };
        let meta = fetched.map_err(|e| io_error(name, &e))?;
        if let Some(format) = p.get("c") {
            out.push_str(&stat_format(format, name, &meta)?);
            out.push('\n');
            continue;
        }
        let shown = match std::fs::read_link(&path) {
            Ok(target) if meta.file_type().is_symlink() => {
                format!("{name} -> {}", target.display())
            }
            _ => name.clone(),
        };
        out.push_str(&format!("  File: {shown}\n"));
        out.push_str(&format!("  Size: {}\n", meta.len()));
        out.push_str(&format!("  Type: {}\n", type_name(&meta)));
        out.push_str(&format!(
            "  Mode: ({:04o}/{})\n",
            mode_bits(&meta),
            mode_string(&meta)
        ));
        if let Some(secs) = mtime_secs(&meta) {
            out.push_str(&format!("Modify: {} ({secs})\n", format_utc(secs)));
        }
    }
    Ok(UtilOutput::ok(out))
}

// ---------------------------------------------------------------- du

struct Du {
    apparent: bool,
    all: bool,
    max_depth: usize,
    entries: usize,
    truncated: bool,
    lines: Vec<(u64, String)>,
}

#[cfg(unix)]
fn disk_bytes(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn disk_bytes(meta: &Metadata) -> u64 {
    meta.len()
}

fn du_walk(path: &Path, shown: &str, depth: usize, du: &mut Du) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = if du.apparent {
        meta.len()
    } else {
        disk_bytes(&meta)
    };
    du.entries += 1;
    if du.entries > DU_ENTRY_LIMIT || depth > DU_DEPTH_LIMIT {
        du.truncated = true;
        return total;
    }
    if meta.is_dir() {
        if let Ok(read_dir) = std::fs::read_dir(path) {
            let mut names: Vec<std::ffi::OsString> = read_dir
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect();
            names.sort();
            for name in names {
                let child_shown =
                    format!("{}/{}", shown.trim_end_matches('/'), name.to_string_lossy());
                total += du_walk(&path.join(&name), &child_shown, depth + 1, du);
            }
        }
        if depth <= du.max_depth {
            du.lines.push((total, shown.to_string()));
        }
    } else if du.all && depth <= du.max_depth {
        du.lines.push((total, shown.to_string()));
    }
    total
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < 4 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{:.0}{}", value.ceil(), UNITS[unit])
    }
}

/// `du [-s] [-h] [-b] [-a] [-d N] [PATH...]`
pub fn du(args: &[String], _stdin: Option<&[u8]>, cwd: &Path) -> UtilResult {
    const SPEC: &[Flag] = &[
        sw("s", 's', "summarize"),
        sw("h", 'h', "human-readable"),
        sw("b", 'b', "bytes"),
        sw("a", 'a', "all"),
        sw("apparent", '\0', "apparent-size"),
        val("d", 'd', "max-depth"),
    ];
    let p = parse(args, SPEC, false)?;
    let max_depth = match (p.has("s"), p.get("d")) {
        (true, _) => 0,
        (false, Some(d)) => parse_usize("max depth", d)?,
        (false, None) => usize::MAX,
    };
    let mut du = Du {
        apparent: p.has("b") || p.has("apparent"),
        all: p.has("a"),
        max_depth,
        entries: 0,
        truncated: false,
        lines: Vec::new(),
    };
    let operands: Vec<String> = if p.operands.is_empty() {
        vec![".".to_string()]
    } else {
        p.operands.clone()
    };
    for name in &operands {
        let path = resolve(cwd, name);
        std::fs::symlink_metadata(&path).map_err(|e| io_error(name, &e))?;
        let before = du.lines.len();
        let total = du_walk(&path, name, 0, &mut du);
        if du.lines.len() == before {
            du.lines.push((total, name.clone()));
        }
    }
    let mut out = String::new();
    for (bytes, shown) in &du.lines {
        let size = if p.has("b") {
            bytes.to_string()
        } else if p.has("h") {
            human(*bytes)
        } else {
            bytes.div_ceil(1024).to_string()
        };
        out.push_str(&format!("{size}\t{shown}\n"));
    }
    let mut result = UtilOutput::ok(out);
    if du.truncated {
        result.stderr = format!("du: stopped after {DU_ENTRY_LIMIT} entries; totals are partial\n");
    }
    Ok(result)
}
