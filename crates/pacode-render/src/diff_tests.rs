use super::*;
use crate::{ColorMode, RenderOptions, Theme};

// The colour mode is passed explicitly rather than left to detection:
// `with_theme` resolves the mode from the ambient terminal and, in `Mono`,
// *replaces the whole theme with `Theme::mono()`* — after which a later
// `color_mode(..)` flips the flag back but cannot bring the colours with it.
// These helpers therefore used to pass or fail depending on the terminal the
// suite happened to run in, while never actually testing truecolor.
fn truecolor_opts(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::truecolor(), ColorMode::Truecolor)
}

fn ansi_opts(width: u16) -> RenderOptions {
    RenderOptions::with_theme_color(width, false, Theme::ansi(), ColorMode::Ansi)
}

fn line_text(l: &Line) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn diff_stat_counts_added_and_removed() {
    let old = "line 1\nline 2\nline 3\n";
    let new = "line 1\nmodified line 2\nline 3\nline 4\n";
    let (_, stat) = unified_diff(old, new, "f", 3);
    assert_eq!(stat.added, 2);
    assert_eq!(stat.removed, 1);
}

/// A rewrite of every other line of a large file is the Myers worst case
/// (D ≈ N); without the timeout it runs for minutes. With it the diff comes
/// back promptly and stays exact: every changed line is still counted.
#[test]
fn huge_rewrite_diff_is_time_bounded_and_exact() {
    let lines = 60_000;
    let old: String = (0..lines).map(|i| format!("line {i}\n")).collect();
    let new: String = (0..lines)
        .map(|i| {
            if i % 2 == 0 {
                format!("changed {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    let started = std::time::Instant::now();
    let (text, stat) = unified_diff(&old, &new, "big.txt", 3);
    let elapsed = started.elapsed();
    assert!(
        elapsed < DIFF_TIMEOUT * 20,
        "diff took {elapsed:?}, budget {DIFF_TIMEOUT:?}"
    );
    assert!(stat.added as usize >= lines / 2);
    assert_eq!(stat.added, stat.removed);
    assert!(text.contains("+changed 0"));
    assert!(text.contains("-line 0"));
}

#[test]
fn unified_diff_generates_header() {
    let old = "fn main() {}\n";
    let new = "fn main() {\n    println!(\"hi\");\n}\n";
    let (diff, _) = unified_diff(old, new, "src/main.rs", 3);
    assert!(diff.contains("--- a/src/main.rs"));
    assert!(diff.contains("+++ b/src/main.rs"));
    assert!(diff.contains("@@"));
    assert!(diff.contains("+    println!(\"hi\");"));
}

#[test]
fn rows_have_numbers_signs_and_syntax() {
    let opts = truecolor_opts(80);
    let diff = "--- a/t.rs\n+++ b/t.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n }\n";
    let lines = render_diff(diff, &opts);
    assert_eq!(lines.len(), 7);

    // Headers faint, hunk dim.
    assert_eq!(lines[0].spans[0].style, opts.theme.faint);
    assert_eq!(lines[1].spans[0].style, opts.theme.faint);
    assert_eq!(lines[2].spans[0].style, opts.theme.dim);

    // Context row: `1   fn main() {` — number faint, blank sign, syntax.
    assert_eq!(line_text(&lines[3]), "1   fn main() {");
    assert_eq!(lines[3].spans[0].style, opts.theme.faint);

    // Removed row: old number, `−` sign, band pads to the right edge.
    assert_eq!(line_text(&lines[4]).trim_end(), "2 −     let x = 1;");
    // Keyword `let` keeps its syntax foreground over the band (fg survives
    // `band.patch`).
    assert!(
        lines[4]
            .spans
            .iter()
            .any(|s| s.style.fg == opts.theme.violet.fg && s.content.as_ref() == "let")
    );
    // Added row: new number, `+` sign.
    assert_eq!(line_text(&lines[5]).trim_end(), "2 +     let x = 2;");
    // Trailing context.
    assert_eq!(line_text(&lines[6]), "3   }");
}

#[test]
fn band_fills_to_the_right_edge_in_truecolor() {
    let opts = truecolor_opts(40);
    let diff = "--- a/t.rs\n+++ b/t.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    let lines = render_diff(diff, &opts);
    let del = &lines[3];
    let add = &lines[4];
    // Every span of a changed row carries the band colour, and the row is
    // padded out to the full width.
    assert!(del.spans.iter().all(|s| s.style.bg.is_some()));
    assert!(add.spans.iter().all(|s| s.style.bg.is_some()));
    let del_bg = del.spans[0].style.bg.unwrap();
    let add_bg = add.spans[0].style.bg.unwrap();
    assert_ne!(del_bg, add_bg);
    assert_eq!(display_width(&line_text(del)), 40);
}

#[test]
fn ansi_mode_drops_the_band_keeps_sign_and_highlight() {
    let opts = ansi_opts(80);
    let diff = "--- a/t.rs\n+++ b/t.rs\n@@ -1,1 +1,1 @@\n-let x = 1\n+let y = 2\n";
    let lines = render_diff(diff, &opts);
    let del = &lines[3];
    let add = &lines[4];
    // No background anywhere.
    assert!(del.spans.iter().all(|s| s.style.bg.is_none()));
    assert!(add.spans.iter().all(|s| s.style.bg.is_none()));
    // Sign keeps its colour, code keeps syntax colours (ANSI palette).
    assert_eq!(add.spans[1].style, opts.theme.green);
    assert_eq!(del.spans[1].style, opts.theme.red);
    assert!(
        add.spans
            .iter()
            .any(|s| s.style == opts.theme.violet && s.content.as_ref() == "let")
    );
}

#[test]
fn mono_mode_keeps_only_the_sign() {
    let opts = RenderOptions::with_theme(80, false, Theme::ansi()).color_mode(ColorMode::Mono);
    let diff = "--- a/t.rs\n+++ b/t.rs\n@@ -1,1 +1,1 @@\n-let x = 1\n+let y = 2\n";
    let lines = render_diff(diff, &opts);
    let del = &lines[3];
    // Sign present, code uncoloured.
    assert_eq!(del.spans[1].content.as_ref(), "−");
    assert!(del.spans.iter().skip(3).all(|s| s.style == opts.theme.fg));
}

#[test]
fn long_lines_are_cut_not_wrapped() {
    let opts = truecolor_opts(20);
    let diff = "--- a/t.rs\n+++ b/t.rs\n@@ -1,1 +1,1 @@\n-a very long removed line\n+a very long added line here\n";
    let lines = render_diff(diff, &opts);
    assert_eq!(lines.len(), 5, "no wrapping — one line per diff row");
    assert_eq!(display_width(&line_text(&lines[3])), 20);
    assert!(line_text(&lines[3]).ends_with('…'));
    assert_eq!(display_width(&line_text(&lines[4])), 20);
}

#[test]
fn long_context_runs_fold() {
    let opts = truecolor_opts(80);
    let ctx: String = (1..=10).map(|i| format!(" ctx {i}\n")).collect();
    let diff = format!("--- a/t\n+++ b/t\n@@ -1,13 +1,13 @@\n{ctx}-old\n+new\n ctx a\n ctx b\n");
    let lines = render_diff(&diff, &opts);
    let text: Vec<String> = lines.iter().map(line_text).collect();
    // 10 ctx lines → 3 kept + a fold marker of 7.
    assert!(text.iter().any(|l| l.contains("7 lines collapsed")));
    assert!(text.iter().any(|l| l.contains("ctrl+o to expand")));
    // …and expanded mode shows them all.
    let all = render_diff_ex(&diff, &opts, true);
    assert!(!all.iter().map(line_text).any(|l| l.contains("collapsed")));
}

#[test]
fn unknown_language_renders_without_highlight() {
    let opts = ansi_opts(80);
    let diff = "--- a/t.cobol\n+++ b/t.cobol\n@@ -1,1 +1,1 @@\n-OLD LINE\n+NEW LINE\n";
    let lines = render_diff(diff, &opts);
    // Code is plain fg; only the sign is coloured.
    let add = &lines[4];
    assert!(add.spans.iter().skip(3).all(|s| s.style == opts.theme.fg));
}

#[test]
fn a_removed_header_like_line_inside_a_hunk_is_content() {
    // `@@ -1,2 +1,2` spends both counters, so `--- x` is a removed line.
    let opts = ansi_opts(80);
    let diff = "--- a/t\n+++ b/t\n@@ -1,2 +1,2 @@\n--- x\n+-- y\n";
    let lines = render_diff(diff, &opts);
    assert_eq!(lines[3].spans[1].content.as_ref(), "−");
    assert_eq!(line_text(&lines[3]), "1 − -- x");
}
