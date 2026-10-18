use base64::Engine as _;

use super::*;

fn init_store() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    pacode_image::store::init(dir.path().to_path_buf());
}

fn png_b64() -> String {
    let img =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(20, 10, image::Rgb([5, 5, 5])));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode");
    base64::engine::general_purpose::STANDARD.encode(out)
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
