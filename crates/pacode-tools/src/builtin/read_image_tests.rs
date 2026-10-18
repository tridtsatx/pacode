use super::*;

fn png() -> Vec<u8> {
    let img =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(12, 6, image::Rgb([1, 2, 3])));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode");
    out
}

#[tokio::test]
async fn text_is_not_treated_as_an_image() {
    assert!(image_output(b"fn main() {}", "a.rs").await.is_none());
}

#[tokio::test]
async fn png_becomes_an_attached_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    pacode_image::store::init(dir.path().to_path_buf());
    let out = image_output(&png(), "shot.png").await.expect("image");
    assert!(!out.is_error);
    assert_eq!(out.images.len(), 1);
    assert_eq!(
        (out.images[0].width, out.images[0].height),
        (Some(12), Some(6))
    );
    assert!(out.content.contains("attached below"));
}

#[tokio::test]
async fn corrupt_image_is_a_tool_error() {
    let mut bad = png();
    bad.truncate(40);
    let out = image_output(&bad, "bad.png").await.expect("image branch");
    assert!(out.is_error, "{}", out.content);
}

#[test]
fn webp_needs_the_webp_tag() {
    assert!(looks_like_image(b"RIFF\0\0\0\0WEBPVP8 "));
    assert!(!looks_like_image(b"RIFF\0\0\0\0WAVEfmt "));
}
