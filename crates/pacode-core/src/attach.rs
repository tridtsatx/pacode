//! Client attachments -> stored images.
//!
//! The daemon prepares every attachment (decode, downscale, re-encode, cap), stores the
//! bytes once in the content-addressed image store and keeps only the [`ImageRef`] in
//! the message and transcript.

use pacode_types::{Attachment, ImageRef, MAX_IMAGES_PER_MESSAGE};

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

#[cfg(test)]
#[path = "attach_tests.rs"]
mod attach_tests;
