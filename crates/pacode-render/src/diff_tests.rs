use super::*;
use crate::RenderOptions;

#[test]
fn diff_stat_counts_added_and_removed() {
    let old = "line 1\nline 2\nline 3\n";
    let new = "line 1\nmodified line 2\nline 3\nline 4\n";
    let stat = diff_stat(old, new);
    assert_eq!(stat.added, 2);
    assert_eq!(stat.removed, 1);
}

#[test]
fn unified_diff_generates_header() {
    let old = "fn main() {}\n";
    let new = "fn main() {\n    println!(\"hi\");\n}\n";
    let diff = unified_diff(old, new, "src/main.rs", 3);
    assert!(diff.contains("--- a/src/main.rs"));
    assert!(diff.contains("+++ b/src/main.rs"));
    assert!(diff.contains("@@"));
    assert!(diff.contains("+    println!(\"hi\");"));
}

#[test]
fn render_diff_styles_lines_and_truncates() {
    let opts = RenderOptions::new(20, false);
    let diff = "--- a/test.rs\n+++ b/test.rs\n@@ -1,1 +1,2 @@\n-old long line that will be truncated\n+new line\n context";
    let lines = render_diff(diff, &opts);
    assert_eq!(lines.len(), 6);

    // Header lines styled faint
    assert_eq!(lines[0].spans[0].style, opts.theme.faint);
    assert_eq!(lines[1].spans[0].style, opts.theme.faint);

    // Hunk header styled dim
    assert_eq!(lines[2].spans[0].style, opts.theme.dim);

    // Deleted line styled red and truncated to 20 with ellipsis
    assert_eq!(lines[3].spans[0].style, opts.theme.red);
    assert_eq!(lines[3].to_string(), "-old long line that…");
    assert_eq!(lines[3].to_string().chars().count(), 20);

    // Added line styled green
    assert_eq!(lines[4].spans[0].style, opts.theme.green);
    assert_eq!(lines[4].to_string(), "+new line");

    // Context line styled fg
    assert_eq!(lines[5].spans[0].style, opts.theme.fg);
    assert_eq!(lines[5].to_string(), " context");
}
