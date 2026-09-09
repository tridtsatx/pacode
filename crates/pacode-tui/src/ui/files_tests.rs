use super::*;

#[test]
fn test_is_image_file_extensions() {
    assert!(is_image_file("photo.png"));
    assert!(is_image_file("PHOTO.PNG"));
    assert!(is_image_file("image.jpg"));
    assert!(is_image_file("IMAGE.JPEG"));
    assert!(is_image_file("animation.gif"));
    assert!(is_image_file("pic.webp"));
    assert!(is_image_file("bitmap.bmp"));

    assert!(!is_image_file("code.rs"));
    assert!(!is_image_file("document.pdf"));
    assert!(!is_image_file("archive.tar.gz"));
    assert!(!is_image_file("no_extension"));
    assert!(!is_image_file(""));
}

#[test]
fn test_truncate_left() {
    assert_eq!(truncate_left("short.rs", 20, true), "short.rs");
    assert_eq!(
        truncate_left("path/to/very/long/file.rs", 10, true),
        "...file.rs"
    );
}
