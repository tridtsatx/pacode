use super::*;

fn png_of(img: &DynamicImage) -> Vec<u8> {
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .unwrap();
    out
}

fn flat(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30])))
}

fn noisy(w: u32, h: u32) -> DynamicImage {
    let mut img = image::RgbImage::new(w, h);
    let mut state = 0x1234_5678u32;
    for p in img.pixels_mut() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *p = image::Rgb([(state >> 8) as u8, (state >> 16) as u8, (state >> 24) as u8]);
    }
    DynamicImage::ImageRgb8(img)
}

#[test]
fn small_flat_image_stays_png_and_keeps_size() {
    let out = prepare(&png_of(&flat(40, 20))).unwrap();
    assert_eq!(out.mime, "image/png");
    assert_eq!((out.width, out.height), (40, 20));
}

#[test]
fn large_image_is_downscaled_to_long_side_cap() {
    let out = prepare(&png_of(&flat(3136, 1000))).unwrap();
    assert_eq!(out.width, MAX_LONG_SIDE);
    assert!(out.height <= 500 && out.height >= 498, "{}", out.height);
}

#[test]
fn photo_like_image_becomes_jpeg() {
    let out = prepare(&png_of(&noisy(200, 200))).unwrap();
    assert_eq!(out.mime, "image/jpeg");
    assert_eq!(&out.bytes[..2], &[0xFF, 0xD8]);
}

#[test]
fn transparency_forces_png() {
    let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        8,
        8,
        image::Rgba([1, 2, 3, 128]),
    ));
    let out = prepare(&png_of(&img)).unwrap();
    assert_eq!(out.mime, "image/png");
}

#[test]
fn errors_are_clear() {
    assert!(matches!(prepare(&[]), Err(PrepareError::Empty)));
    assert!(matches!(
        prepare(b"definitely not an image"),
        Err(PrepareError::Unsupported)
    ));
    let big = vec![0u8; MAX_INPUT_BYTES + 1];
    assert!(matches!(
        prepare(&big),
        Err(PrepareError::InputTooLarge { .. })
    ));
}

#[test]
fn oversized_dimensions_are_rejected_before_decoding() {
    let img = image::RgbImage::new(MAX_INPUT_SIDE + 1, 1);
    let bytes = png_of(&DynamicImage::ImageRgb8(img));
    assert!(matches!(
        prepare(&bytes),
        Err(PrepareError::DimensionsTooLarge { .. })
    ));
}

#[test]
fn output_never_exceeds_cap() {
    let out = prepare(&png_of(&noisy(1800, 1800))).unwrap();
    assert!(out.bytes.len() <= MAX_PREPARED_BYTES);
    assert!(out.width.max(out.height) <= MAX_LONG_SIDE);
}

#[test]
fn count_cap() {
    assert!(check_count(MAX_IMAGES).is_ok());
    assert!(matches!(
        check_count(MAX_IMAGES + 1),
        Err(PrepareError::TooMany { .. })
    ));
}

#[test]
fn jpeg_input_is_reencoded_without_metadata_markers() {
    let rgb = noisy(64, 64).to_rgb8();
    let mut jpg = Vec::new();
    {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 90);
        enc.encode(rgb.as_raw(), 64, 64, ExtendedColorType::Rgb8)
            .unwrap();
    }
    let out = prepare(&jpg).unwrap();
    // No APP1 (EXIF) marker right after SOI.
    assert_ne!(&out.bytes[2..4], &[0xFF, 0xE1]);
}

#[test]
fn clean_small_input_is_kept_byte_for_byte() {
    let png = png_of(&flat(40, 20));
    let out = prepare(&png).unwrap();
    assert_eq!(out.bytes, png);
    // Preparing the output again is a no-op (no recompression).
    let again = prepare(&out.bytes).unwrap();
    assert_eq!(again.bytes, out.bytes);
}

#[test]
fn exif_markers_are_detected() {
    assert!(has_exif(
        b"\xff\xd8\xff\xe1\x00\x10Exif\0\0rest",
        ImageFormat::Jpeg
    ));
    assert!(!has_exif(b"\xff\xd8\xff\xe0JFIF", ImageFormat::Jpeg));
    assert!(has_exif(b"..eXIf..", ImageFormat::Png));
}
