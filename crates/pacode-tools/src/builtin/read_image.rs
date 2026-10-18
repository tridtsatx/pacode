//! `read` on an image file: the picture is prepared, stored once in the image store and
//! attached to the tool result (`ToolOutput::images`). The core shows it in the
//! transcript and sends it to the model only when the model accepts images; otherwise
//! it replaces it with a one-line note.

use pacode_types::ImageRef;

use crate::ToolOutput;

#[cfg(test)]
#[path = "read_image_tests.rs"]
mod read_image_tests;

const IMAGE_MAGIC: [&[u8]; 4] = [b"\x89PNG\r\n\x1a\n", b"\xff\xd8\xff", b"GIF8", b"RIFF"];

/// Whether `bytes` start like a png, jpeg, gif or webp (RIFF....WEBP) file.
fn looks_like_image(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"RIFF") {
        return bytes.get(8..12) == Some(b"WEBP".as_slice());
    }
    IMAGE_MAGIC.iter().any(|m| bytes.starts_with(m))
}

/// `Some(output)` when `bytes` is an image (by content, not by name), else `None`
/// so the text path runs.
pub(super) async fn image_output(bytes: &[u8], shown_path: &str) -> Option<ToolOutput> {
    if !looks_like_image(bytes) {
        return None;
    }
    let owned = bytes.to_vec();
    let prepared = tokio::task::spawn_blocking(move || pacode_image::prepare(&owned)).await;
    let prepared = match prepared {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            return Some(ToolOutput::error(format!(
                "cannot read image {shown_path}: {e}"
            )));
        }
        Err(e) => return Some(ToolOutput::error(format!("image task failed: {e}"))),
    };
    let hash = match pacode_image::store::put(&prepared.bytes, prepared.mime) {
        Ok(h) => h,
        Err(e) => {
            return Some(ToolOutput::error(format!(
                "cannot store image {shown_path}: {e}"
            )));
        }
    };
    let (w, h) = (prepared.width, prepared.height);
    let image = ImageRef {
        hash,
        mime: prepared.mime.to_string(),
        width: Some(w),
        height: Some(h),
        name: Some(shown_path.to_string()),
    };
    Some(
        ToolOutput::text(format!(
            "Image file {shown_path} ({w}x{h}, {}). The picture is attached below.",
            prepared.mime
        ))
        .with_title(format!("Read {shown_path}"))
        .with_preview(format!("image {w}x{h}"))
        .with_images(vec![image]),
    )
}
