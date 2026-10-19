use base64::Engine as _;

use super::*;

fn init_store() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    pacode_image::store::init(dir.path().to_path_buf());
}

fn png_bytes() -> Vec<u8> {
    let img =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(20, 10, image::Rgb([5, 5, 5])));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode");
    out
}

fn png_b64() -> String {
    base64::engine::general_purpose::STANDARD.encode(png_bytes())
}

fn stored_ref(source: &ImageSource) -> ImageRef {
    source
        .image_ref()
        .unwrap_or_else(|| panic!("expected a stored image, got {source:?}"))
}

#[tokio::test]
async fn a_path_image_is_stored_and_outlives_its_file() {
    init_store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paste-1.png");
    std::fs::write(&path, png_bytes()).unwrap();

    let out = store_sources(vec![ImageSource::Path(path.clone())], true)
        .await
        .expect("stored");
    std::fs::remove_file(&path).unwrap();

    let image = stored_ref(&out[0]);
    assert_eq!((image.width, image.height), (Some(20), Some(10)));
    assert_eq!(
        image.name.as_deref(),
        Some(path.display().to_string().as_str())
    );
    assert!(
        pacode_image::store::load(&image.hash).is_ok(),
        "the bytes live in the store, not in the deleted file"
    );
}

#[tokio::test]
async fn raw_bytes_are_stored_and_a_stored_image_passes_through() {
    init_store();
    let stored = ImageSource::stored(&ImageRef {
        hash: "a".repeat(64),
        mime: "image/png".into(),
        width: Some(1),
        height: Some(1),
        name: None,
    });
    let out = store_sources(
        vec![
            ImageSource::Bytes {
                mime: "image/png".into(),
                data: png_bytes(),
            },
            stored.clone(),
        ],
        true,
    )
    .await
    .expect("stored");
    assert_eq!(out.len(), 2);
    assert!(pacode_image::store::load(&stored_ref(&out[0]).hash).is_ok());
    assert_eq!(out[1], stored, "already stored: untouched");
}

#[tokio::test]
async fn an_unreadable_image_fails_only_a_vision_model() {
    init_store();
    let missing = ImageSource::Path("/nonexistent/pacode-test/gone.png".into());
    let err = store_sources(vec![missing.clone()], true)
        .await
        .expect_err("a vision model would get nothing");
    assert!(err.to_string().contains("gone.png"), "{err}");

    // Without vision the image is only ever its text placeholder: no error.
    let out = store_sources(vec![missing.clone()], false)
        .await
        .expect("kept");
    assert_eq!(out, vec![missing]);
}

#[tokio::test]
async fn attachments_are_prepared_and_stored() {
    init_store();
    let refs = store_attachments(vec![Attachment {
        mime: "image/png".into(),
        data: png_b64(),
        name: Some("a.png".into()),
    }])
    .await
    .expect("stored");
    assert_eq!(refs.len(), 1);
    assert_eq!((refs[0].width, refs[0].height), (Some(20), Some(10)));
    assert_eq!(refs[0].name.as_deref(), Some("a.png"));
    assert!(pacode_image::store::load(&refs[0].hash).is_ok());
}

#[tokio::test]
async fn bad_attachment_names_the_file() {
    init_store();
    let err = store_attachments(vec![Attachment {
        mime: "image/png".into(),
        data: "bm90IGFuIGltYWdl".into(),
        name: Some("bad.png".into()),
    }])
    .await
    .expect_err("must fail");
    assert!(err.to_string().contains("bad.png"), "{err}");
}

#[tokio::test]
async fn too_many_attachments_rejected() {
    let one = Attachment {
        mime: "image/png".into(),
        data: png_b64(),
        name: None,
    };
    let err = store_attachments(vec![one; MAX_IMAGES_PER_MESSAGE + 1])
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("at most"), "{err}");
}
