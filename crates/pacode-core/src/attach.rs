//! Client attachments and prompt images -> stored images.
//!
//! The daemon prepares every attachment (decode, downscale, re-encode, cap), stores the
//! bytes once in the content-addressed image store and keeps only the [`ImageRef`] in
//! the message and transcript. Prompt images that arrive as a file path or raw bytes
//! (`Request::UserMessage.images`) get the same treatment at admission, so the message
//! never depends on a file the client may delete (a TUI paste lives in a temp file that
//! goes away when the TUI exits).

use pacode_types::{Attachment, ImageRef, ImageSource, MAX_IMAGES_PER_MESSAGE};

use crate::CoreError;

/// Prepare and store `attachments`, in order. Nothing is stored when any one fails
/// validation up front (count); a decode failure names the offending image.
pub(crate) async fn store_attachments(
    attachments: Vec<Attachment>,
) -> Result<Vec<ImageRef>, CoreError> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    if attachments.len() > MAX_IMAGES_PER_MESSAGE {
        return Err(CoreError::Invalid(format!(
            "at most {MAX_IMAGES_PER_MESSAGE} images per message"
        )));
    }
    tokio::task::spawn_blocking(move || store_blocking(attachments))
        .await
        .map_err(|e| CoreError::Invalid(format!("image task failed: {e}")))?
}

fn store_blocking(attachments: Vec<Attachment>) -> Result<Vec<ImageRef>, CoreError> {
    let mut out = Vec::with_capacity(attachments.len());
    for (i, attachment) in attachments.into_iter().enumerate() {
        let label = attachment
            .name
            .clone()
            .unwrap_or_else(|| format!("image {}", i + 1));
        let prepared = pacode_image::prepare::prepare_base64(&attachment.data)
            .map_err(|e| CoreError::Invalid(format!("{label}: {e}")))?;
        let hash = pacode_image::store::put(&prepared.bytes, prepared.mime)
            .map_err(|e| CoreError::Invalid(format!("{label}: {e}")))?;
        out.push(ImageRef {
            hash,
            mime: prepared.mime.to_string(),
            width: Some(prepared.width),
            height: Some(prepared.height),
            name: attachment.name,
        });
    }
    Ok(out)
}

/// Turn every `Path` / `Bytes` prompt image into an [`ImageSource::Stored`]: read once,
/// prepared and put in the image store. `Stored` sources pass through, and so does
/// everything when no image store is set up (nothing to store into).
///
/// `vision` is whether the session model accepts images. With it, an image that cannot
/// be read or prepared is an error naming it, before anything is admitted. Without it
/// the image only ever reaches the model as its text placeholder, so a failure keeps
/// the source as it came instead of refusing the message.
pub(crate) async fn store_sources(
    images: Vec<ImageSource>,
    vision: bool,
) -> Result<Vec<ImageSource>, CoreError> {
    if pacode_image::store::global().is_none()
        || images.iter().all(|source| source.image_ref().is_some())
    {
        return Ok(images);
    }
    tokio::task::spawn_blocking(move || {
        images
            .into_iter()
            .enumerate()
            .map(|(i, source)| match store_source(&source) {
                Ok(Some(image)) => Ok(ImageSource::stored(&image)),
                Ok(None) => Ok(source),
                Err(e) if vision => Err(CoreError::Invalid(format!(
                    "{}: {e}",
                    source_label(&source, i)
                ))),
                Err(e) => {
                    log::warn!(
                        "prompt image {} kept unstored (model has no vision): {e}",
                        source_label(&source, i)
                    );
                    Ok(source)
                }
            })
            .collect::<Result<Vec<_>, CoreError>>()
    })
    .await
    .map_err(|e| CoreError::Invalid(format!("image task failed: {e}")))?
}

/// How an error names a prompt image: its path, else its position.
fn source_label(source: &ImageSource, index: usize) -> String {
    match source {
        ImageSource::Path(path) => path.display().to_string(),
        ImageSource::Bytes { .. } | ImageSource::Stored { .. } => format!("image {}", index + 1),
    }
}

/// Prepare and store one source; `None` for one already stored.
fn store_source(source: &ImageSource) -> Result<Option<ImageRef>, String> {
    let (prepared, name) = match source {
        ImageSource::Stored { .. } => return Ok(None),
        ImageSource::Path(path) => {
            // Checked before reading: a path can name anything, of any size.
            let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
            if size > pacode_image::prepare::MAX_INPUT_BYTES as u64 {
                return Err(format!(
                    "image file is {size} bytes, over the {} byte limit",
                    pacode_image::prepare::MAX_INPUT_BYTES
                ));
            }
            let raw = std::fs::read(path).map_err(|e| e.to_string())?;
            (
                pacode_image::prepare::prepare(&raw).map_err(|e| e.to_string())?,
                Some(path.display().to_string()),
            )
        }
        ImageSource::Bytes { data, .. } => (
            pacode_image::prepare::prepare(data).map_err(|e| e.to_string())?,
            None,
        ),
    };
    let hash =
        pacode_image::store::put(&prepared.bytes, prepared.mime).map_err(|e| e.to_string())?;
    Ok(Some(ImageRef {
        hash,
        mime: prepared.mime.to_string(),
        width: Some(prepared.width),
        height: Some(prepared.height),
        name,
    }))
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod attach_tests;
