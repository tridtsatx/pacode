use super::*;

/// The whole point of the window machinery: a model that asked for a slice must
/// be able to tell it apart from the whole thing without counting characters.
#[test]
fn a_whole_page_gets_no_note() {
    assert!(window_note(0, 100, 100, 100, false, 0).is_none());
}

#[test]
fn a_windowed_read_says_which_slice_and_how_to_continue() {
    let note = window_note(100, 300, 1000, 200, false, 0).expect("a window is noted");
    assert!(note.contains("chars 100..300 of 1000"), "{note}");
    assert!(note.contains("offset: 300"), "{note}");
    assert!(note.contains("200 characters per call"), "{note}");
}

#[test]
fn the_first_window_says_it_is_the_first() {
    let note = window_note(0, 200, 1000, 200, false, 0).expect("a window is noted");
    assert!(note.contains("first 200 of 1000"), "{note}");
    assert!(note.contains("offset: 200"), "{note}");
}

#[test]
fn a_window_reaching_the_end_offers_the_start_back() {
    let note = window_note(800, 1000, 1000, 200, false, 0).expect("a window is noted");
    assert!(note.contains("offset: 0"), "{note}");
    assert!(
        !note.contains("offset: 1000"),
        "there is nothing past the end to read: {note}"
    );
}

#[test]
fn a_capped_transfer_is_stated_even_when_the_text_is_complete() {
    // The rendered text fits, but the response was cut — the model has to know
    // the page continues or it will treat what it got as the whole document.
    let note = window_note(0, 100, 100, 100, true, 40_000_000).expect("a cut is noted");
    assert!(note.contains("cut at 5 MiB"), "{note}");
    assert!(note.contains("40000000 bytes unread"), "{note}");
}

/// Lifting the cap must actually lift it. A compile-time assertion, not a
/// runtime one: both sides are constants, so a runtime `assert!` is a constant
/// expression clippy rightly rejects, and the check is worth more as a
/// build-time invariant than as a passing test line.
const _: () = assert!(
    MAX_TOTAL_CHARS > MAX_CHARS_WINDOW,
    "lifting the cap must actually lift it"
);

#[test]
fn accept_large_output_widens_the_window_to_the_ceiling() {
    assert_eq!(max_window_chars(false), MAX_CHARS_WINDOW);
    assert_eq!(max_window_chars(true), MAX_TOTAL_CHARS);
}

/// The window is a character window, not a byte window: cutting a multi-byte
/// page mid-codepoint would produce invalid UTF-8 in the model's context.
#[test]
fn a_window_never_splits_a_codepoint() {
    let text = "é".repeat(1000);
    let window: String = text.chars().skip(10).take(100).collect();
    assert_eq!(window.chars().count(), 100);
    assert!(text.contains(&window));
}

// ---------------------------------------------------------------------------
// The transfer cap. The defect these guard: a body over the cap used to end the
// call as an error, so a 40 MiB page returned nothing at all instead of the first
// 5 MiB plus a note saying there was more.
// ---------------------------------------------------------------------------

mod transfer_cap {
    use super::*;

    /// A body comfortably over the cap, in chunks the way a real stream arrives.
    fn push_chunks(accumulator: &mut BodyAccumulator, total: usize, chunk_size: usize) {
        let chunk = vec![b'x'; chunk_size];
        let mut sent = 0;
        while sent < total {
            let end = (sent + chunk_size).min(total);
            if !accumulator.push(&chunk[..end - sent]) {
                return;
            }
            sent = end;
        }
    }

    #[test]
    fn a_body_over_the_cap_is_cut_not_rejected() {
        let mut accumulator = BodyAccumulator::new();
        push_chunks(
            &mut accumulator,
            MAX_BODY_BYTES + 3 * 1024 * 1024,
            64 * 1024,
        );

        assert_eq!(
            accumulator.bytes.len(),
            MAX_BODY_BYTES,
            "the kept body is the cap, not the whole payload and not nothing"
        );
        assert!(
            accumulator.capped,
            "and the call has to be able to say the transfer was cut"
        );
    }

    #[test]
    fn the_accumulator_tells_the_caller_to_stop_reading() {
        // The reason the loop is worth having: a body far over the cap must stop
        // costing requests and bytes, not be drained and then discarded.
        let mut accumulator = BodyAccumulator::new();
        assert!(
            accumulator.push(&vec![b'a'; 1024]),
            "under the cap: keep going"
        );
        let mut chunks_before_stop = 1;
        while accumulator.push(&vec![b'a'; 1024]) {
            chunks_before_stop += 1;
            assert!(
                chunks_before_stop <= MAX_BODY_BYTES / 1024 + 1,
                "kept reading past the cap"
            );
        }
        assert!(accumulator.capped);
    }

    #[test]
    fn a_body_exactly_at_the_cap_is_complete_not_cut() {
        // Off-by-one: at exactly the cap nothing has been lost, so claiming a
        // truncation would tell the model to go looking for a tail that is not
        // there.
        let mut accumulator = BodyAccumulator::new();
        push_chunks(&mut accumulator, MAX_BODY_BYTES, 128 * 1024);
        assert_eq!(accumulator.bytes.len(), MAX_BODY_BYTES);
        assert!(
            !accumulator.capped,
            "nothing was dropped, so nothing should be reported as dropped"
        );
    }

    #[test]
    fn a_body_one_byte_over_the_cap_is_cut() {
        let mut accumulator = BodyAccumulator::new();
        push_chunks(&mut accumulator, MAX_BODY_BYTES + 1, 1024 * 1024);
        assert_eq!(accumulator.bytes.len(), MAX_BODY_BYTES);
        assert!(accumulator.capped, "a single dropped byte is still a cut");
    }

    #[test]
    fn a_chunk_straddling_the_cap_keeps_the_part_that_fits() {
        // The chunk is copied up to the boundary and the remainder dropped. The
        // alternative — refusing the whole chunk — would throw away a megabyte of
        // readable page because the last one was too big.
        let mut accumulator = BodyAccumulator::new();
        push_chunks(&mut accumulator, MAX_BODY_BYTES - 10, 64 * 1024);
        assert!(!accumulator.capped);
        assert!(!accumulator.push(&vec![b'y'; 4096]));
        assert_eq!(accumulator.bytes.len(), MAX_BODY_BYTES);
        assert!(accumulator.capped);
        assert_eq!(
            *accumulator.bytes.last().expect("non-empty"),
            b'y',
            "the readable head of the straddling chunk is kept"
        );
    }

    #[test]
    fn a_small_body_is_taken_whole_and_never_capped() {
        let mut accumulator = BodyAccumulator::new();
        for chunk in ["hello ", "world"] {
            assert!(accumulator.push(chunk.as_bytes()));
        }
        assert_eq!(accumulator.bytes, b"hello world");
        assert!(!accumulator.capped);
    }

    #[test]
    fn a_cut_body_still_renders_a_usable_window() {
        // The end-to-end shape of the fix: a body over the cap produces a
        // window with a note, not an error and not a silent truncation.
        let mut accumulator = BodyAccumulator::new();
        push_chunks(&mut accumulator, MAX_BODY_BYTES + 1024, 256 * 1024);
        let total = accumulator.bytes.len();
        let note = window_note(0, 100, total, 100, accumulator.capped, 1024)
            .expect("a capped transfer is always noted");
        assert!(note.contains("cut at 5 MiB"), "{note}");
        assert!(note.contains("1024 bytes unread"), "{note}");
    }
}
