//! Prepares user-supplied images for the model: decode, strip metadata, downscale,
//! re-encode and cap. Pure (no filesystem, no network).
//!
//! * decoded with the `image` crate (png, jpeg, webp, gif, bmp); animated gifs use the
//!   first frame; the EXIF orientation is applied, then every piece of metadata is
//!   dropped by re-encoding the pixels only;
//! * long side scaled down to [`MAX_LONG_SIDE`] (providers rescale above ~1568 px anyway);
//! * PNG for screenshots, line art and anything with transparency, JPEG quality
//!   [`JPEG_QUALITY`] for photos;
//! * the encoded result is capped at [`MAX_PREPARED_BYTES`] (checked before base64).

use std::io::Cursor;

use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageFormat};

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod prepare_tests;

/// Longest side of a prepared image, in pixels.
pub const MAX_LONG_SIDE: u32 = 1568;
/// Cap on the encoded image, before base64.
pub const MAX_PREPARED_BYTES: usize = 5 * 1024 * 1024;
/// Cap on the raw input file.
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Cap on the input side length (decompression-bomb guard, checked before decoding).
pub const MAX_INPUT_SIDE: u32 = 12_000;
/// Cap on input pixels (decompression-bomb guard).
pub const MAX_INPUT_PIXELS: u64 = 80_000_000;
/// JPEG quality used for photos.
pub const JPEG_QUALITY: u8 = 85;
/// Images per user message.
pub const MAX_IMAGES: usize = 8;

/// A prepared image ready to store and send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    /// `image/png` or `image/jpeg`.
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("image is empty")]
    Empty,
    #[error("image file is {size} bytes, over the {limit} byte limit")]
    InputTooLarge { size: usize, limit: usize },
    #[error("unsupported image format (supported: png, jpeg, webp, gif, bmp)")]
    Unsupported,
    #[error("image is {width}x{height} pixels, over the input limit")]
    DimensionsTooLarge { width: u32, height: u32 },
    #[error("cannot decode image: {0}")]
    Decode(String),
    #[error("cannot encode image: {0}")]
    Encode(String),
    #[error("image is still {size} bytes after downscaling, over the {limit} byte limit")]
    OutputTooLarge { size: usize, limit: usize },
    #[error("at most {max} images per message")]
    TooMany { max: usize },
}

/// Fail when a message would carry more than [`MAX_IMAGES`] images.
pub fn check_count(count: usize) -> Result<(), PrepareError> {
    if count > MAX_IMAGES {
        return Err(PrepareError::TooMany { max: MAX_IMAGES });
    }
    Ok(())
}

/// Prepare a base64 payload (a leading `data:...;base64,` prefix is tolerated).
pub fn prepare_base64(data: &str) -> Result<Prepared, PrepareError> {
    use base64::Engine as _;
    let payload = match data.split_once(";base64,") {
        Some((prefix, rest)) if prefix.starts_with("data:") => rest,
        Some(_) | None => data,
    };
    // Cap the decoded size before allocating it (base64 is 4/3 of the payload).
    if payload.len() / 4 * 3 > MAX_INPUT_BYTES + 3 {
        return Err(PrepareError::InputTooLarge {
            size: payload.len() / 4 * 3,
            limit: MAX_INPUT_BYTES,
        });
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| PrepareError::Decode(format!("invalid base64: {e}")))?;
    prepare(&raw)
}

/// Prepare raw image bytes (see module docs).
pub fn prepare(raw: &[u8]) -> Result<Prepared, PrepareError> {
    if raw.is_empty() {
        return Err(PrepareError::Empty);
    }
    if raw.len() > MAX_INPUT_BYTES {
        return Err(PrepareError::InputTooLarge {
            size: raw.len(),
            limit: MAX_INPUT_BYTES,
        });
    }
    let format = image::guess_format(raw).map_err(|_| PrepareError::Unsupported)?;
    if !matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::WebP
            | ImageFormat::Gif
            | ImageFormat::Bmp
    ) {
        return Err(PrepareError::Unsupported);
    }

    let reader = image::ImageReader::with_format(Cursor::new(raw), format);
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| PrepareError::Decode(e.to_string()))?;
    if w == 0
        || h == 0
        || w > MAX_INPUT_SIDE
        || h > MAX_INPUT_SIDE
        || u64::from(w) * u64::from(h) > MAX_INPUT_PIXELS
    {
        return Err(PrepareError::DimensionsTooLarge {
            width: w,
            height: h,
        });
    }

    let mut decoder = image::ImageReader::with_format(Cursor::new(raw), format)
        .into_decoder()
        .map_err(|e| PrepareError::Decode(e.to_string()))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| PrepareError::Decode(e.to_string()))?;
    img.apply_orientation(orientation);

    // Already prepared (by the TUI) or small and clean: keep the bytes as they are,
    // so a second pass never recompresses a JPEG.
    if matches!(orientation, image::metadata::Orientation::NoTransforms)
        && w.max(h) <= MAX_LONG_SIDE
        && raw.len() <= MAX_PREPARED_BYTES
        && !has_exif(raw, format)
        && let Some(mime) = match format {
            ImageFormat::Jpeg => Some("image/jpeg"),
            // A PNG photo is better re-encoded as JPEG; only keep flat art and alpha.
            ImageFormat::Png if img.has_alpha() || looks_like_line_art(&img) => Some("image/png"),
            _ => None,
        }
    {
        return Ok(Prepared {
            bytes: raw.to_vec(),
            mime,
            width: w,
            height: h,
        });
    }

    if img.width().max(img.height()) > MAX_LONG_SIDE {
        img = img.resize(
            MAX_LONG_SIDE,
            MAX_LONG_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }

    let mut encoded = encode_best(&img)?;
    // A very detailed image can still exceed the cap: shrink in steps.
    let mut side = img.width().max(img.height());
    while encoded.0.len() > MAX_PREPARED_BYTES && side > 256 {
        side = side * 3 / 4;
        img = img.resize(side, side, image::imageops::FilterType::Lanczos3);
        encoded = encode_best(&img)?;
    }
    if encoded.0.len() > MAX_PREPARED_BYTES {
        return Err(PrepareError::OutputTooLarge {
            size: encoded.0.len(),
            limit: MAX_PREPARED_BYTES,
        });
    }
    Ok(Prepared {
        bytes: encoded.0,
        mime: encoded.1,
        width: img.width(),
        height: img.height(),
    })
}

fn encode_best(img: &DynamicImage) -> Result<(Vec<u8>, &'static str), PrepareError> {
    if img.has_alpha() || looks_like_line_art(img) {
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|e| PrepareError::Encode(e.to_string()))?;
        // PNG of a busy opaque image can dwarf a JPEG: fall back when it is too big.
        if out.len() <= MAX_PREPARED_BYTES || img.has_alpha() {
            return Ok((out, "image/png"));
        }
    }
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
        enc.encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|e| PrepareError::Encode(e.to_string()))?;
    }
    Ok((out, "image/jpeg"))
}

/// Few distinct colours on a coarse sample: screenshots, diagrams, flat graphics.
fn looks_like_line_art(img: &DynamicImage) -> bool {
    let thumb = img.thumbnail(64, 64).to_rgb8();
    let mut seen = std::collections::HashSet::new();
    for p in thumb.pixels() {
        seen.insert((p[0] >> 3, p[1] >> 3, p[2] >> 3));
        if seen.len() > 96 {
            return false;
        }
    }
    true
}

/// Whether the container carries an EXIF block (JPEG APP1 `Exif`, PNG `eXIf` chunk).
fn has_exif(raw: &[u8], format: ImageFormat) -> bool {
    let window = &raw[..raw.len().min(256 * 1024)];
    let needle: &[u8] = match format {
        ImageFormat::Jpeg => b"Exif\0\0",
        ImageFormat::Png => b"eXIf",
        _ => return false,
    };
    window.windows(needle.len()).any(|w| w == needle)
}
