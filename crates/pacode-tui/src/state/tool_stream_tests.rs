use super::*;

/// One line of a chatty tool's output — the unit a 10 MB `cargo build` log
/// arrives in.
const LOG_LINE: &str = "   Compiling pacode-tui v0.1.0 (/home/pacode/crates/pacode-tui)\n";

#[test]
fn a_ten_megabyte_tool_keeps_one_capped_window_of_the_newest_output() {
    let mut streams = ToolStreams::default();
    let mut pushed = 0usize;
    while pushed < 10 * 1024 * 1024 {
        streams.push(7, LOG_LINE);
        pushed += LOG_LINE.len();
    }
    let view = streams.get(7).expect("stream");

    assert_eq!(view.text.chars().count(), MAX_TOOL_STREAM_CHARS);
    assert!(view.dropped > 9 * 1024 * 1024, "dropped {}", view.dropped);
    // The newest bytes are what a live body must show; a head-kept window
    // would show the first lines of the log forever.
    assert!(
        view.text.starts_with("   Compiling"),
        "{:?}",
        &view.text[..40]
    );
    assert!(
        view.text.ends_with("pacode-tui)\n"),
        "{:?}",
        &view.text[view.text.len() - 40..]
    );
}

#[test]
fn one_chunk_larger_than_the_cap_is_capped_too() {
    let mut streams = ToolStreams::default();
    streams.push(1, &"x".repeat(MAX_TOOL_STREAM_CHARS * 3));

    let view = streams.get(1).expect("stream");
    assert_eq!(view.text.chars().count(), MAX_TOOL_STREAM_CHARS);
    assert_eq!(view.dropped, MAX_TOOL_STREAM_CHARS * 2);
}

#[test]
fn the_cap_cuts_on_a_character_boundary() {
    let mut streams = ToolStreams::default();
    // Two bytes per char, from a non-ASCII codepoint: a byte-indexed cut would
    // land mid-codepoint. The letter itself is ASCII-only on purpose — the
    // repository forbids Cyrillic in source string literals, and the width is
    // what matters here, not the alphabet.
    streams.push(2, &"é".repeat(MAX_TOOL_STREAM_CHARS + 5));

    let view = streams.get(2).expect("stream");
    assert_eq!(view.text.chars().count(), MAX_TOOL_STREAM_CHARS);
    assert_eq!(view.dropped, 5);
    assert!(view.text.chars().all(|c| c == 'é'));
}

#[test]
fn the_map_drops_its_oldest_call_past_the_bound() {
    let mut streams = ToolStreams::default();
    let last = MAX_TOOL_STREAMS as u64 + 2;
    for cell_id in 0..=last {
        streams.push(cell_id, "line\n");
    }

    // Ids are monotonic, so the oldest calls are the ones that go: `last + 1`
    // pushes against a bound of `MAX_TOOL_STREAMS` leaves exactly three
    // evicted, and the call just pushed is never one of them.
    for gone in 0..3 {
        assert_eq!(
            streams.get(gone),
            None,
            "id {gone} should have been evicted"
        );
    }
    assert!(streams.get(3).is_some(), "the oldest survivor");
    assert!(
        streams.get(last).is_some(),
        "the newest is never the victim"
    );
}
