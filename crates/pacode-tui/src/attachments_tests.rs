use super::*;

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
