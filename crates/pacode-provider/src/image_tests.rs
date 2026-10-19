use pacode_types::{ContentBlock, ImageSource, Message, Role};

use super::{MAX_IMAGE_BYTES, image_fallback_text, load_image_data, text_with_image_fallbacks};

#[test]
fn fallback_text_has_expand_shape() {
    // Bytes source: size is known inline.
    let src = ImageSource::Bytes {
        mime: "image/png".into(),
        data: vec![0; 42],
    };
    assert_eq!(image_fallback_text(&src), "[image: image/png, 42 bytes]");

    // Path source: `[image file: <name> (<n> bytes)]` like expand.rs.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, [1u8, 2, 3]).unwrap();
    let src = ImageSource::Path(file.clone());
    assert_eq!(
        image_fallback_text(&src),
        format!("[image file: {} (3 bytes)]", file.display())
    );

    // Missing file: degrade to the bare placeholder, never panic.
    let src = ImageSource::Path(dir.path().join("gone.png"));
    assert_eq!(
        image_fallback_text(&src),
        format!("[image file: {}]", dir.path().join("gone.png").display())
    );
}

#[test]
fn text_fallbacks_append_after_text() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.png");
    std::fs::write(&file, [9u8; 7]).unwrap();
    let msg = Message::new(
        Role::User,
        vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            ContentBlock::Image {
                source: ImageSource::Path(file.clone()),
            },
        ],
    );
    assert_eq!(
        text_with_image_fallbacks(&msg),
        format!("look\n[image file: {} (7 bytes)]", file.display())
    );

    // Image-only message still yields the placeholder.
    let msg = Message::new(
        Role::User,
        vec![ContentBlock::Image {
            source: ImageSource::Bytes {
                mime: "image/webp".into(),
                data: vec![1, 2],
            },
        }],
    );
    assert_eq!(
        text_with_image_fallbacks(&msg),
        "[image: image/webp, 2 bytes]"
    );
}

#[test]
fn load_image_data_from_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pic.JPG");
    std::fs::write(&file, [0xFF, 0xD8, 0xFF]).unwrap();

    let (mime, b64) = load_image_data(&ImageSource::Path(file), MAX_IMAGE_BYTES).expect("loads");
    assert_eq!(mime, "image/jpeg");
    assert_eq!(b64, "/9j/");
}

#[test]
fn load_image_data_from_bytes() {
    let src = ImageSource::Bytes {
        mime: "image/gif".into(),
        data: vec![1, 2, 3, 4],
    };
    let (mime, b64) = load_image_data(&src, MAX_IMAGE_BYTES).expect("loads");
    assert_eq!(mime, "image/gif");
    assert_eq!(b64, "AQIDBA==");
}

#[test]
fn load_image_data_rejects_oversize_and_unknown() {
    let big = ImageSource::Bytes {
        mime: "image/png".into(),
        data: vec![0; 128],
    };
    assert!(load_image_data(&big, 64).is_none());

    let dir = tempfile::tempdir().unwrap();
    let txt = dir.path().join("note.txt");
    std::fs::write(&txt, b"hi").unwrap();
    assert!(load_image_data(&ImageSource::Path(txt), MAX_IMAGE_BYTES).is_none());
    assert!(
        load_image_data(
            &ImageSource::Path(dir.path().join("missing.png")),
            MAX_IMAGE_BYTES
        )
        .is_none()
    );
}
