use super::*;
use ratatui::style::{Color, Style};

#[test]
fn display_width_counts_columns() {
    assert_eq!(display_width("hello"), 5);
    assert_eq!(display_width("привет"), 6);
    assert_eq!(display_width("hello\tworld"), 14); // tab is 4 spaces
    assert_eq!(display_width("hello\x00world"), 10); // control char is 0
    assert_eq!(display_width("你好"), 4); // CJK chars are width 2 each
}

#[test]
fn truncate_to_width_cases() {
    assert_eq!(truncate_to_width("hello", 10, true), "hello");
    assert_eq!(truncate_to_width("hello world", 8, true), "hello w…");
    assert_eq!(truncate_to_width("hello world", 8, false), "hello wo");
    assert_eq!(truncate_to_width("привет", 4, true), "при…");
    assert_eq!(truncate_to_width("привет", 4, false), "прив");
    assert_eq!(truncate_to_width("你好世界", 5, true), "你好…");
    assert_eq!(truncate_to_width("你好世界", 5, false), "你好");
    assert_eq!(truncate_to_width("hello", 0, true), "");
    assert_eq!(truncate_to_width("hello", 1, true), "…");
}

#[test]
fn cyrillic_text_wraps_by_grapheme_at_width_10() {
    let text = "Привет прекрасный мир!";
    // "Привет" = 6, " " = 1, "прекрасный" = 10, " " = 1, "мир!" = 4
    let wrapped = wrap_text(text, 10);
    assert_eq!(wrapped, vec!["Привет", "прекрасный", "мир!"]);
    for line in &wrapped {
        assert!(display_width(line) <= 10);
    }
}

#[test]
fn long_word_split() {
    // 21 Cyrillic chars
    let text = "достопримечательность";
    let wrapped = wrap_text(text, 10);
    assert_eq!(wrapped, vec!["достоприме", "чательност", "ь"]);
    for line in &wrapped {
        assert!(display_width(line) <= 10);
    }

    // English long word
    let text_en = "abcdefghijklmnopqrstuvwxyz";
    let wrapped_en = wrap_text(text_en, 10);
    assert_eq!(wrapped_en, vec!["abcdefghij", "klmnopqrst", "uvwxyz"]);
}

#[test]
fn wide_chars_wrap() {
    let text = "你好世界，很高兴见到你";
    // Each CJK char is 2 cells. Width 10 fits 5 CJK characters.
    let wrapped = wrap_text(text, 10);
    assert_eq!(wrapped, vec!["你好世界，", "很高兴见到", "你"]);
    for line in &wrapped {
        assert!(display_width(line) <= 10);
    }
}

#[test]
fn wrap_text_empty_and_newlines() {
    assert_eq!(wrap_text("", 10), vec![""]);
    assert_eq!(wrap_text("a\nb", 10), vec!["a", "b"]);
    assert_eq!(wrap_text("a\n\nb", 10), vec!["a", "", "b"]);
    assert_eq!(wrap_text("hello\n", 10), vec!["hello", ""]);
}

#[test]
fn wrap_line_preserves_span_styles_and_applies_indent() {
    let s1 = Style::default().fg(Color::Red);
    let s2 = Style::default().fg(Color::Blue);
    let line = Line::from(vec![
        Span::styled("hello ", s1),
        Span::styled("world today", s2),
    ]);
    let wrapped = wrap_line(line, 8, 2);
    assert_eq!(wrapped.len(), 3);
    assert_eq!(wrapped[0].to_string(), "hello");
    assert_eq!(wrapped[0].spans[0].style, s1);

    assert_eq!(wrapped[1].to_string(), "  world");
    // Indent is unstyled
    assert_eq!(wrapped[1].spans[0].content, "  ");
    assert_eq!(wrapped[1].spans[0].style, Style::default());
    // "world" preserves s2
    assert_eq!(wrapped[1].spans[1].content, "world");
    assert_eq!(wrapped[1].spans[1].style, s2);

    assert_eq!(wrapped[2].to_string(), "  today");
    assert_eq!(wrapped[2].spans[0].content, "  ");
    assert_eq!(wrapped[2].spans[0].style, Style::default());
    assert_eq!(wrapped[2].spans[1].content, "today");
    assert_eq!(wrapped[2].spans[1].style, s2);
}
