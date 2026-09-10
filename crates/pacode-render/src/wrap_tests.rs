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
fn display_width_combining_marks_and_zwj_sequences() {
    // Single combining mark on base character occupies 1 cell
    assert_eq!(display_width("e\u{0301}"), 1);
    // Multiple combining marks on one base character occupy 1 cell
    assert_eq!(display_width("a\u{0300}\u{0315}"), 1);
    // Woman technologist emoji ZWJ sequence occupies 2 cells
    assert_eq!(display_width("👩‍💻"), 2);
    // Family emoji ZWJ sequence (4 emojis + 3 ZWJs) occupies 2 cells
    assert_eq!(display_width("👨‍👩‍👧‍👦"), 2);
    // Mixed text with emoji ZWJ sequence
    assert_eq!(display_width("dev: 👩‍💻 done"), 5 + 2 + 5);
}

#[test]
fn truncate_to_width_boundary_and_over_width_cases() {
    // Exactly at width
    assert_eq!(truncate_to_width("abcde", 5, true), "abcde");
    assert_eq!(truncate_to_width("abcde", 5, false), "abcde");

    // One cell over width
    assert_eq!(truncate_to_width("abcdef", 5, true), "abcd…");
    assert_eq!(truncate_to_width("abcdef", 5, false), "abcde");

    // Token much longer than width
    assert_eq!(truncate_to_width("abcdefghij", 5, true), "abcd…");
    assert_eq!(truncate_to_width("abcdefghij", 5, false), "abcde");

    // Wide (CJK) grapheme never placed half over boundary
    // "你好世界" is 4 CJK chars (2 cells each = 8 cells).
    // At width 5 with ellipsis (target 4): "你好…" (2+2+1 = 5 cells)
    assert_eq!(truncate_to_width("你好世界", 5, true), "你好…");
    // At width 5 without ellipsis: "你好" (4 cells, 3rd char "世" needs cell 5 and 6, so excluded)
    assert_eq!(truncate_to_width("你好世界", 5, false), "你好");
    assert!(display_width(&truncate_to_width("你好世界", 5, false)) <= 5);
    // At width 4 with ellipsis (target 3): "你…" (2+1 = 3 cells, "好" cannot fit in 1 cell)
    assert_eq!(truncate_to_width("你好世界", 4, true), "你…");
    assert!(display_width(&truncate_to_width("你好世界", 4, true)) <= 4);
    // At width 3 with ellipsis (target 2): "你…" (2+1 = 3 cells)
    assert_eq!(truncate_to_width("你好世界", 3, true), "你…");
    assert!(display_width(&truncate_to_width("你好世界", 3, true)) <= 3);
}

#[test]
fn wrap_line_boundary_and_over_width_cases() {
    // Line exactly at width (5 cells)
    let line = Line::from("abcde");
    let wrapped = wrap_line(line, 5, 0);
    assert_eq!(wrapped.len(), 1);
    assert_eq!(wrapped[0].to_string(), "abcde");

    // Token one cell over width (6 cells at width 5)
    let line = Line::from("abcdef");
    let wrapped = wrap_line(line, 5, 0);
    assert_eq!(wrapped.len(), 2);
    assert_eq!(wrapped[0].to_string(), "abcde");
    assert_eq!(wrapped[1].to_string(), "f");

    // Token longer than width (10 cells at width 4)
    let line = Line::from("0123456789");
    let wrapped = wrap_line(line, 4, 0);
    assert_eq!(wrapped.len(), 3);
    assert_eq!(wrapped[0].to_string(), "0123");
    assert_eq!(wrapped[1].to_string(), "4567");
    assert_eq!(wrapped[2].to_string(), "89");

    // Wide (CJK) grapheme never placed half over boundary
    // Line of width 5: "a" (1 cell) + "你好" (4 cells) = 5 cells.
    // Adding "世界" (4 cells) forces wrap at CJK boundary.
    let line = Line::from("a你好世界");
    let wrapped = wrap_line(line, 5, 0);
    for l in &wrapped {
        assert!(display_width(&l.to_string()) <= 5);
    }
    assert_eq!(wrapped[0].to_string(), "a你好");
    assert_eq!(wrapped[1].to_string(), "世界");
}

#[test]
fn cyrillic_paragraph_at_wide_dialog_area_does_not_break_after_single_char() {
    let text = "Игры нет — можно собирать полностью. Запускаю clippy и тесты в фоне, параллельно проверяю правила кода.";
    let line = Line::from(text);
    let wrapped = wrap_line(line, 120, 0);
    // At width 120, the whole 105-column paragraph must remain on a single line
    assert_eq!(wrapped.len(), 1);
    assert_eq!(wrapped[0].to_string(), text);
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
