use super::*;

#[test]
fn keeps_prefix_and_suffix_when_over_budget() {
    let mut buf = HeadTailBuffer::new(5, 5);

    buf.push(b"0123456789");
    assert_eq!(buf.total_bytes(), 10);
    assert_eq!(buf.head(), b"01234");
    assert_eq!(buf.tail(), b"56789");

    // Exceeds max by 2; keep head+tail and omit the middle.
    buf.push(b"ab");
    assert_eq!(buf.total_bytes(), 12);
    assert_eq!(buf.head(), b"01234");
    assert_eq!(buf.tail(), b"789ab");

    let rendered = buf.render(1000);
    assert!(rendered.starts_with("01234"));
    assert!(rendered.ends_with("789ab"));
    assert!(rendered.contains("2 bytes omitted"));
    assert_eq!(rendered, "01234\n[... 2 bytes omitted ...]\n789ab");
}

#[test]
fn max_bytes_zero_drops_everything() {
    let mut buf = HeadTailBuffer::new(0, 0);
    buf.push(b"abc");

    assert_eq!(buf.total_bytes(), 3);
    assert_eq!(buf.head(), b"");
    assert_eq!(buf.tail(), b"");
    assert_eq!(buf.render(1000), "\n[... 3 bytes omitted ...]\n");
}

#[test]
fn head_budget_zero_keeps_only_tail() {
    let mut buf = HeadTailBuffer::new(0, 1);
    buf.push(b"abc");

    assert_eq!(buf.total_bytes(), 3);
    assert_eq!(buf.head(), b"");
    assert_eq!(buf.tail(), b"c");
    assert_eq!(buf.render(1000), "\n[... 2 bytes omitted ...]\nc");
}

#[test]
fn chunk_larger_than_tail_budget_keeps_only_tail_end() {
    let mut buf = HeadTailBuffer::new(5, 5);
    buf.push(b"0123456789");
    buf.push(b"ABCDEFGHIJK");

    assert_eq!(buf.head(), b"01234");
    assert_eq!(buf.tail(), b"GHIJK");
    assert_eq!(buf.total_bytes(), 21);
    let rendered = buf.render(1000);
    assert!(rendered.starts_with("01234"));
    assert!(rendered.ends_with("GHIJK"));
    assert!(rendered.contains("11 bytes omitted"));
}

#[test]
fn fills_head_then_tail_across_multiple_chunks() {
    let mut buf = HeadTailBuffer::new(5, 5);

    buf.push(b"01");
    buf.push(b"234");
    assert_eq!(buf.head(), b"01234");
    assert_eq!(buf.tail(), b"");

    buf.push(b"567");
    buf.push(b"89");
    assert_eq!(buf.head(), b"01234");
    assert_eq!(buf.tail(), b"56789");
    assert_eq!(buf.total_bytes(), 10);
    assert_eq!(buf.render(1000), "0123456789");

    buf.push(b"a");
    assert_eq!(buf.head(), b"01234");
    assert_eq!(buf.tail(), b"6789a");
    assert_eq!(buf.total_bytes(), 11);
    assert_eq!(buf.render(1000), "01234\n[... 1 bytes omitted ...]\n6789a");
}

#[test]
fn tail_lines_extraction() {
    let mut buf = HeadTailBuffer::new(100, 100);
    buf.push(b"line 1\nline 2\nline 3\nline 4\n");
    assert_eq!(buf.tail_lines(0), Vec::<String>::new());
    assert_eq!(buf.tail_lines(2), vec!["line 3", "line 4"]);
    assert_eq!(
        buf.tail_lines(10),
        vec!["line 1", "line 2", "line 3", "line 4"]
    );

    // Trailing partial line counts as a line
    let mut buf2 = HeadTailBuffer::new(100, 100);
    buf2.push(b"alpha\nbeta\ngamma");
    assert_eq!(buf2.tail_lines(2), vec!["beta", "gamma"]);
    assert_eq!(buf2.tail_lines(1), vec!["gamma"]);
}

#[test]
fn tail_lines_when_omitted() {
    let mut buf = HeadTailBuffer::new(5, 12);
    buf.push(b"head_\nskip1\nskip2\ntail1\ntail2\n");
    let lines = buf.tail_lines(2);
    assert_eq!(lines, vec!["tail1", "tail2"]);
}
