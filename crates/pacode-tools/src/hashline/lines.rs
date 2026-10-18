//! Line splitting that remembers each line's terminator.

/// One line of a file: text without terminator, plus the terminator it had
/// (`"\n"`, `"\r\n"`, or `""` for an unterminated last line).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Line<'a> {
    pub text: &'a str,
    pub eol: &'a str,
}

/// Split like `str::lines` but keep terminators. `""` has no lines; `"\n"` has one empty line.
pub(crate) fn split_lines(content: &str) -> Vec<Line<'_>> {
    content
        .split_inclusive('\n')
        .map(|seg| {
            if let Some(text) = seg.strip_suffix("\r\n") {
                Line { text, eol: "\r\n" }
            } else if let Some(text) = seg.strip_suffix('\n') {
                Line { text, eol: "\n" }
            } else {
                Line { text: seg, eol: "" }
            }
        })
        .collect()
}

/// Terminator for inserted lines: CRLF only when CRLF lines outnumber LF lines.
pub(crate) fn dominant_eol(lines: &[Line<'_>]) -> &'static str {
    let crlf = lines.iter().filter(|l| l.eol == "\r\n").count();
    let lf = lines.iter().filter(|l| l.eol == "\n").count();
    if crlf > lf { "\r\n" } else { "\n" }
}
