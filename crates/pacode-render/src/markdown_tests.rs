use super::*;
use crate::{ColorMode, RenderOptions};

#[test]
fn fenced_code_highlights_known_language() {
    let md = "```rust\nfn main() {}\n```";
    let opts = RenderOptions::new(40, false).color_mode(ColorMode::Truecolor);
    let lines = render_markdown(md, &opts);
    assert_eq!(lines.len(), 1);
    // `fn` carries the keyword colour inside the `│ ` gutter line.
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "fn" && s.style.fg == opts.theme.violet.fg)
    );
}

#[test]
fn fenced_code_unknown_language_and_mono_stay_plain() {
    for md in ["```cobol\nIDENT DIV.\n```", "```rust\nlet x = 1\n```"] {
        let mono = RenderOptions::new(40, false).color_mode(ColorMode::Mono);
        let lines = render_markdown(md, &mono);
        assert!(
            lines[0]
                .spans
                .iter()
                .skip(1)
                .all(|s| s.style == mono.theme.fg)
        );
    }
    // Unknown language is plain even in truecolor.
    let opts = RenderOptions::new(40, false).color_mode(ColorMode::Truecolor);
    let lines = render_markdown("```cobol\nIDENT DIV.\n```", &opts);
    assert!(
        lines[0]
            .spans
            .iter()
            .skip(1)
            .all(|s| s.style == opts.theme.fg)
    );
}

#[test]
fn paragraph_list_code_block_at_width_40() {
    let md = "\
This is a paragraph of text that should wrap nicely at width 40.

- Item one
- Item two

```rust
fn main() {
    println!(\"hello\");
}
```";

    let opts = RenderOptions::new(40, false);
    let lines = render_markdown(md, &opts);
    let strings: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let expected = vec![
        "This is a paragraph of text that should",
        "wrap nicely at width 40.",
        "",
        "• Item one",
        "• Item two",
        "",
        "│ fn main() {",
        "│     println!(\"hello\");",
        "│ }",
    ];

    assert_eq!(strings, expected);
}

#[test]
fn table_rendering() {
    let md = "\
| Name | Age |
| --- | --- |
| Alice | 30 |
| Bob | 25 |";

    let opts = RenderOptions::new(40, false);
    let lines = render_markdown(md, &opts);
    let strings: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let expected = vec!["Name  │ Age", "──────┼────", "Alice │ 30 ", "Bob   │ 25 "];

    assert_eq!(strings, expected);
}

#[test]
fn table_overflow_cuts_with_ellipsis() {
    let md = "\
| VeryLongColumnHeaderOne | VeryLongColumnHeaderTwo |
| --- | --- |
| ValueOneLong | ValueTwoLong |";

    let opts = RenderOptions::new(16, false);
    let lines = render_markdown(md, &opts);
    // At width 16, column widths are constrained and truncated with ellipsis
    for line in &lines {
        assert!(display_width(&line.to_string()) <= 16);
    }
    let strings: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    assert!(strings.iter().any(|s| s.contains('…')));
}

#[test]
fn split_stable_tail_with_open_fence() {
    let text = "Intro paragraph\n\n```rust\nfn main() {\n\n    let x = 1;\n";
    let (stable, tail) = split_stable_tail(text);
    assert_eq!(stable, "Intro paragraph\n\n");
    assert_eq!(tail, "```rust\nfn main() {\n\n    let x = 1;\n");
}

#[test]
fn split_stable_tail_with_only_open_fence() {
    let text = "```rust\nfn main() {\n\n    let x = 1;\n";
    let (stable, tail) = split_stable_tail(text);
    assert_eq!(stable, "");
    assert_eq!(tail, text);
}

#[test]
fn split_stable_tail_closed_fence_and_table() {
    let text = "Para 1\n\n```rust\nfn main() {}\n```\n\nPara 2";
    let (stable, tail) = split_stable_tail(text);
    assert_eq!(stable, "Para 1\n\n```rust\nfn main() {}\n```\n\n");
    assert_eq!(tail, "Para 2");

    let table_text = "Intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let (stable, tail) = split_stable_tail(table_text);
    assert_eq!(stable, "Intro\n\n");
    assert_eq!(tail, "| a | b |\n|---|---|\n| 1 | 2 |\n");
}

#[test]
fn blockquote_rendering() {
    let md = "> Quoted line one\n> Quoted line two";
    let opts = RenderOptions::new(40, false);
    let lines = render_markdown(md, &opts);
    let strings: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    assert!(strings[0].starts_with("▎ "));
}

#[test]
fn links_styled_cyan_with_dim_url_when_different() {
    let md = "[example](https://example.com) and [https://same.com](https://same.com)";
    let opts = RenderOptions::new(80, false);
    let lines = render_markdown(md, &opts);
    assert_eq!(lines.len(), 1);

    let line = &lines[0];
    let str_rep = line.to_string();
    assert!(str_rep.contains("example (https://example.com)"));
    assert!(str_rep.contains("https://same.com"));
    assert!(!str_rep.contains("https://same.com (https://same.com)"));
}
