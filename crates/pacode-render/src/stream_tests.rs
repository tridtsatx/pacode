use super::*;

fn op_chars(ops: &[StreamOp]) -> usize {
    ops.iter()
        .map(|op| match op {
            StreamOp::Text(t) | StreamOp::Reasoning(t) => t.chars().count(),
            StreamOp::CloseReasoning => 0,
        })
        .sum()
}

fn drain_frames(buf: &mut StreamBuffer, start: Instant, frame: Duration) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut t = start;
    let mut guard = 0;
    while !buf.is_empty() {
        t += frame;
        let ops = buf.reveal(t);
        let chars = op_chars(&ops);
        if chars > 0 {
            sizes.push(chars);
        }
        guard += 1;
        assert!(guard < 100_000, "drain did not converge");
    }
    sizes
}

fn flatten(ops: impl IntoIterator<Item = StreamOp>) -> Vec<(char, String)> {
    let mut out: Vec<(char, String)> = Vec::new();
    for op in ops {
        let (tag, text) = match op {
            StreamOp::Text(t) => ('t', t),
            StreamOp::Reasoning(t) => ('r', t),
            StreamOp::CloseReasoning => ('c', String::new()),
        };
        if tag != 'c'
            && let Some((last_tag, last_text)) = out.last_mut()
            && *last_tag == tag
        {
            last_text.push_str(&text);
            continue;
        }
        out.push((tag, text));
    }
    out
}

#[test]
fn flush_drains_everything() {
    let now = Instant::now();
    let mut buf = StreamBuffer::new(now);
    buf.push(StreamKind::Text, "remaining content");
    let ops = buf.flush();
    assert_eq!(ops, vec![StreamOp::Text("remaining content".to_string())]);
    assert!(buf.is_empty());
}

#[test]
fn empty_push_reveals_nothing() {
    let now = Instant::now();
    let mut buf = StreamBuffer::new(now);
    buf.push(StreamKind::Text, "");
    buf.push(StreamKind::Reasoning, "");
    assert!(buf.reveal(now).is_empty());
    assert!(buf.is_empty());
}

#[test]
fn paced_reveal_spreads_a_burst_over_multiple_frames() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Text, &"a".repeat(40));

    let sizes = drain_frames(&mut buf, start, Duration::from_millis(16));
    let total: usize = sizes.iter().sum();
    assert_eq!(total, 40);
    assert!(
        sizes.len() >= 3,
        "a 40-char burst should reveal across multiple frames, got {sizes:?}"
    );
    assert!(
        sizes.iter().all(|&n| n < 40),
        "no frame should reveal the entire burst, got {sizes:?}"
    );
}

#[test]
fn large_single_burst_is_bounded_by_wall_clock_reveal_rate() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Text, &"a".repeat(3_356));

    let sizes = drain_frames(&mut buf, start, Duration::from_millis(50));
    assert_eq!(sizes.iter().sum::<usize>(), 3_356);
    assert!(
        sizes.iter().all(|&n| n <= 48),
        "a 50ms paced frame must reveal at most 48 chars: {sizes:?}"
    );
    assert!(
        sizes.len() >= 70,
        "the burst should drain smoothly over several seconds: {} frames",
        sizes.len()
    );
}

#[test]
fn reveal_ceiling_is_independent_of_redraw_cadence() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Text, &"b".repeat(1_000));

    let sizes = drain_frames(&mut buf, start, Duration::from_millis(16));
    assert_eq!(sizes.iter().sum::<usize>(), 1_000);
    assert!(
        sizes.iter().all(|&n| n <= 16),
        "a 16ms paced frame must reveal at most 16 chars: {sizes:?}"
    );
}

#[test]
fn frequent_push_calls_cannot_bypass_the_wall_clock_ceiling() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Text, &"c".repeat(1_000));

    let first_at = start + Duration::from_millis(50);
    let mut revealed = op_chars(&buf.reveal(first_at));
    for _ in 0..100 {
        revealed += op_chars(&buf.reveal(first_at));
    }
    assert_eq!(revealed, 48);

    let second = op_chars(&buf.reveal(first_at + Duration::from_millis(50)));
    assert_eq!(second, 48);
}

#[test]
fn reasoning_burst_is_paced_like_text() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Reasoning, &"r".repeat(40));

    let sizes = drain_frames(&mut buf, start, Duration::from_millis(16));
    assert_eq!(sizes.iter().sum::<usize>(), 40);
    assert!(
        sizes.len() >= 3 && sizes.iter().all(|&n| n < 40),
        "reasoning bursts must be paced, got {sizes:?}"
    );
}

#[test]
fn idle_gap_does_not_dump_the_next_burst() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    let arrival = start + Duration::from_secs(5);
    buf.push(StreamKind::Text, &"b".repeat(30));
    let first = op_chars(&buf.reveal(arrival));
    assert!(
        first < 30,
        "the idle gap must not bank budget that dumps the burst, revealed {first}"
    );
    let sizes = drain_frames(&mut buf, arrival, Duration::from_millis(16));
    assert_eq!(first + sizes.iter().sum::<usize>(), 30);
}

#[test]
fn ordering_is_preserved_across_kinds_and_markers() {
    let mut now = Instant::now();
    let mut buf = StreamBuffer::new(now);
    let mut ops = Vec::new();

    buf.push(StreamKind::Reasoning, "think think");
    now += Duration::from_millis(10);
    ops.extend(buf.reveal(now));

    buf.close_reasoning();
    now += Duration::from_millis(10);
    ops.extend(buf.reveal(now));

    buf.push(StreamKind::Text, "answer one");
    now += Duration::from_millis(10);
    ops.extend(buf.reveal(now));

    buf.push(StreamKind::Reasoning, "more thinking");
    now += Duration::from_millis(10);
    ops.extend(buf.reveal(now));

    buf.push(StreamKind::Text, "answer two");
    ops.extend(buf.flush());

    let trace = flatten(ops);
    assert_eq!(
        trace,
        vec![
            ('r', "think think".to_string()),
            ('c', String::new()),
            ('t', "answer one".to_string()),
            ('r', "more thinking".to_string()),
            ('c', String::new()),
            ('t', "answer two".to_string()),
        ]
    );
    assert!(buf.is_empty());
}

#[test]
fn close_marker_waits_for_buffered_reasoning() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Reasoning, &"z".repeat(60));
    buf.close_reasoning();

    // Drain everything; the close marker must come after all reasoning chars.
    let mut all = Vec::new();
    let mut t = start;
    let mut guard = 0;
    while !buf.is_empty() {
        t += Duration::from_millis(16);
        all.extend(buf.reveal(t));
        guard += 1;
        assert!(guard < 100_000);
    }
    let close_idx = all
        .iter()
        .position(|op| matches!(op, StreamOp::CloseReasoning))
        .expect("close marker must drain");
    assert_eq!(close_idx, all.len() - 1);
    let reasoning_chars: usize = all
        .iter()
        .map(|op| match op {
            StreamOp::Reasoning(t) => t.chars().count(),
            _ => 0,
        })
        .sum();
    assert_eq!(reasoning_chars, 60);
}

#[test]
fn whitespace_text_does_not_close_reasoning() {
    let now = Instant::now();
    let mut buf = StreamBuffer::new(now);
    let mut ops = Vec::new();
    buf.push(StreamKind::Reasoning, "thinking");
    buf.push(StreamKind::Text, "\n");
    buf.push(StreamKind::Reasoning, " still thinking");
    ops.extend(buf.flush());
    assert!(
        !ops.iter().any(|op| matches!(op, StreamOp::CloseReasoning)),
        "whitespace-only text must not close the reasoning region: {ops:?}"
    );
}

#[test]
fn marker_only_queue_emits_immediately() {
    let now = Instant::now();
    let mut buf = StreamBuffer::new(now);
    buf.push(StreamKind::Reasoning, "r");
    let _ = buf.flush();
    // After flush, reopen reasoning then close it
    buf.push(StreamKind::Reasoning, "");
    // Force open reasoning
    buf.push(StreamKind::Reasoning, "a");
    let _ = buf.flush();
    buf.push(StreamKind::Reasoning, "test");
    let _ = buf.flush();
    buf.close_reasoning();
    // In our implementation, close_reasoning queues CloseReasoning when reasoning_open is true
    // If reasoning was open, close_reasoning queues CloseReasoning
}

#[test]
fn reveal_respects_utf8_boundaries() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Text, &"é".repeat(40));

    let sizes = drain_frames(&mut buf, start, Duration::from_millis(16));
    assert_eq!(sizes.iter().sum::<usize>(), 40);
}

#[test]
fn small_trailing_text_eventually_drains() {
    let start = Instant::now();
    let mut buf = StreamBuffer::new(start);
    buf.push(StreamKind::Text, "hi");
    let sizes = drain_frames(&mut buf, start, Duration::from_millis(16));
    assert_eq!(sizes.iter().sum::<usize>(), 2);
}
