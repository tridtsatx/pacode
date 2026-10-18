use super::*;
use crate::state::AppState;
use pacode_types::Config;

fn real_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        w,
        h,
        image::Rgb([20, 120, 220]),
    ));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode png");
    out
}

fn state() -> AppState {
    AppState::new(Config::default(), "1.0.0".to_string(), 80, 24)
}

#[test]
fn chip_text_shape() {
    assert_eq!(chip_text(1, 1229), "[Image 1 \u{b7} 1.2 KB]");
}

#[test]
fn finds_chips_with_char_offsets() {
    let text = "see [Image 1 \u{b7} 3 B] and [Image 12 \u{b7} 1.5 MB] ok [Image x]";
    let chips = find_chips(text);
    assert_eq!(chips.len(), 2);
    assert_eq!((chips[0].start, chips[0].end, chips[0].n), (4, 19, 1));
    assert_eq!(chips[1].n, 12);
}

#[test]
fn malformed_chips_are_plain_text() {
    for t in [
        "[Image 1]",
        "[Image \u{b7} 3 B]",
        "[Image 1 \u{b7} ]",
        "[image 1 \u{b7} 3 B]",
        "[Image 1 \u{b7} 3 B",
    ] {
        assert!(find_chips(t).is_empty(), "{t}");
    }
}

#[test]
fn backspace_and_delete_ranges() {
    let text = "a [Image 1 \u{b7} 3 B] b";
    let chip = find_chips(text)[0];
    assert_eq!(chip_for_backspace(text, chip.end), Some(chip));
    assert_eq!(chip_for_backspace(text, chip.start + 3), Some(chip));
    assert_eq!(chip_for_backspace(text, chip.start), None);
    assert_eq!(chip_for_delete(text, chip.start), Some(chip));
    assert_eq!(chip_for_delete(text, chip.end), None);
}

#[test]
fn take_for_text_orders_by_appearance_and_consumes() {
    let mut pending = PendingAttachments::default();
    let c1 = pending.add(&real_png(8, 8), Some("a.png".into())).unwrap();
    let c2 = pending.add(&real_png(9, 9), None).unwrap();
    let text = format!("compare {c2} with {c1} and {c2} again");
    let (out, attachments) = pending.take_for_text(&text);
    assert_eq!(out, "compare [Image 1] with [Image 2] and [Image 1] again");
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[1].name.as_deref(), Some("a.png"));
    assert_eq!(attachments[0].mime, "image/png");
    assert!(pending.is_empty());
}

#[test]
fn unknown_chips_stay_literal() {
    let mut pending = PendingAttachments::default();
    let (out, attachments) = pending.take_for_text("old [Image 7 \u{b7} 1 B] text");
    assert_eq!(out, "old [Image 7 \u{b7} 1 B] text");
    assert!(attachments.is_empty());
}

#[test]
fn at_most_eight_pending() {
    let mut pending = PendingAttachments::default();
    for _ in 0..MAX_PENDING {
        pending.add(&real_png(4, 4), None).unwrap();
    }
    assert!(matches!(
        pending.add(&real_png(4, 4), None),
        Err(AttachError::TooMany)
    ));
}

#[test]
fn deleted_chips_free_their_slot() {
    let mut s = state();
    for _ in 0..MAX_PENDING {
        let chip = s.attach_bytes(&real_png(4, 4), None).unwrap();
        s.input.insert_str(&chip);
    }
    assert!(s.attach_bytes(&real_png(4, 4), None).is_err());
    s.input.text.clear();
    s.input.cursor = 0;
    assert!(s.attach_bytes(&real_png(4, 4), None).is_ok());
}

#[test]
fn build_user_message_carries_attachments() {
    let mut s = state();
    let chip = s
        .attach_bytes(&real_png(8, 8), Some("x.png".into()))
        .unwrap();
    let req = s.build_user_message(format!("what is {chip}?"));
    match req {
        Request::UserMessage { text, attachments } => {
            assert_eq!(text, "what is [Image 1]?");
            assert_eq!(attachments.len(), 1);
            assert!(!attachments[0].data.is_empty());
        }
        other => panic!("unexpected {other:?}"),
    }
    // Plain text: no attachments.
    match s.build_user_message("hi".into()) {
        Request::UserMessage { attachments, .. } => assert!(attachments.is_empty()),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn image_extension_detection() {
    assert!(is_image_path("a/b/c.PNG"));
    assert!(is_image_path("x.jpeg"));
    assert!(!is_image_path("x.rs"));
    assert!(!is_image_path("png"));
}

#[test]
fn drag_and_drop_path_forms() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("my shot.png");
    std::fs::write(&file, b"x").unwrap();
    let plain = file.display().to_string();
    assert_eq!(image_path_from_paste(&plain), Some(file.clone()));
    assert_eq!(
        image_path_from_paste(&format!("'{plain}'\n")),
        Some(file.clone())
    );
    assert_eq!(
        image_path_from_paste(&plain.replace(' ', "\\ ")),
        Some(file.clone())
    );
    assert_eq!(
        image_path_from_paste(&format!("file://{}", plain.replace(' ', "%20"))),
        Some(file)
    );
    assert_eq!(image_path_from_paste("two\nlines.png"), None);
    assert_eq!(image_path_from_paste("/definitely/missing.png"), None);
}

#[test]
fn at_candidate_attaches_instead_of_inserting_text() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pic.png"), real_png(10, 10)).unwrap();
    let mut s = state();
    s.cwd = dir.path().to_path_buf();
    s.input.text = "look @pi".to_string();
    s.input.cursor = 8;
    assert!(s.attach_at_candidate(5, 8, "pic.png"));
    assert!(
        s.input.text.starts_with("look [Image 1"),
        "{}",
        s.input.text
    );
    assert!(s.input.text.ends_with("] "));
    assert_eq!(s.attachments.len(), 1);
    // Unreadable file: text stays, caller falls back to `@path`.
    s.input.text = "x @nope.png".to_string();
    assert!(!s.attach_at_candidate(2, 11, "nope.png"));
    assert_eq!(s.input.text, "x @nope.png");
}
