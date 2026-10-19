//! Sixel graphics protocol encoding via `icy_sixel` (quantette-based palette
//! quantization, DCS framing `ESC P … q` … `ESC \` included in the output).

#[cfg(test)]
#[path = "sixel_tests.rs"]
mod sixel_tests;

use crate::render::ImageError;

/// Assumed terminal cell size in pixels for scaling a preview into a cell box.
///
/// Sixel renders at device pixel resolution (P1=9 keeps pixels square), so the
/// only handle we have on the displayed size is the encoded pixel size. Most
/// monospace cells are close to a 1:2 width:height ratio; picking 10x20 keeps
/// the preview aspect right on any terminal whose cell is near that shape.
const CELL_PX_W: u32 = 10;
const CELL_PX_H: u32 = 20;

/// Cap on the encoded pixel area so a large source image cannot blow up the
/// quantization pass; previews are always downscaled into a cell box far below
/// this, the cap is a safety net for callers passing huge cell sizes.
const MAX_PIXELS: u32 = 4_000_000;

/// Encode raw RGBA pixels as a Sixel escape sequence (DCS `ESC P … q` … `ESC \`).
pub fn encode_sixel_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<String, icy_sixel::SixelError> {
    icy_sixel::sixel_encode(
        rgba,
        width as usize,
        height as usize,
        &icy_sixel::EncodeOptions::default(),
    )
}

/// Encode an image into a Sixel escape sequence scaled to fit `cell_w` x `cell_h`
/// terminal cells, preserving aspect ratio.
///
/// Returns an empty string for a zero-sized cell box: there is nothing to draw
/// and emitting an empty image would just waste a frame.
pub fn encode_sixel(
    img: &image::DynamicImage,
    cell_w: u16,
    cell_h: u16,
) -> Result<String, ImageError> {
    if cell_w == 0 || cell_h == 0 || img.width() == 0 || img.height() == 0 {
        return Ok(String::new());
    }

    let box_w = (cell_w as u32).saturating_mul(CELL_PX_W).min(MAX_PIXELS);
    let box_h = (cell_h as u32).saturating_mul(CELL_PX_H).min(MAX_PIXELS);
    let ratio = (box_w as f64 / img.width() as f64).min(box_h as f64 / img.height() as f64);
    // Never upscale: sixel output size grows with pixels and previews only shrink.
    let ratio = ratio.min(1.0);
    let target_w = ((img.width() as f64 * ratio).round() as u32).max(1);
    let target_h = ((img.height() as f64 * ratio).round() as u32).max(1);

    let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
    let rgba = resized.to_rgba8();
    let esc = encode_sixel_rgba(rgba.as_raw(), target_w, target_h)
        .map_err(|e| ImageError::Sixel(e.to_string()))?;
    Ok(esc)
}
