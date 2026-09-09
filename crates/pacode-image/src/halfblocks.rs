//! Half-block terminal preview rendering using UTF-8 `▀` cells.

#[cfg(test)]
#[path = "halfblocks_tests.rs"]
mod halfblocks_tests;

/// One terminal cell in a half-block preview: top pixel and bottom pixel RGB colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PreviewCell {
    pub top: (u8, u8, u8),
    pub bottom: (u8, u8, u8),
}

/// Compute downscaled image dimensions in pixels preserving aspect ratio,
/// constrained within `cell_w` columns and `cell_h * 2` pixel rows.
pub fn compute_halfblock_dimensions(
    orig_w: u32,
    orig_h: u32,
    cell_w: u16,
    cell_h: u16,
) -> (u32, u32) {
    if orig_w == 0 || orig_h == 0 || cell_w == 0 || cell_h == 0 {
        return (0, 0);
    }
    let max_w = cell_w as f64;
    let max_h = (cell_h as f64) * 2.0;
    let ratio_w = max_w / orig_w as f64;
    let ratio_h = max_h / orig_h as f64;
    let ratio = ratio_w.min(ratio_h);

    let new_w = ((orig_w as f64 * ratio).round() as u32).max(1);
    let new_h = ((orig_h as f64 * ratio).round() as u32).max(1);
    (new_w.min(cell_w as u32), new_h.min((cell_h as u32) * 2))
}

/// Render an image into a 2D grid of `PreviewCell`s (outer vector is rows, inner is columns).
pub fn render_halfblocks(
    img: &image::DynamicImage,
    cell_w: u16,
    cell_h: u16,
) -> Vec<Vec<PreviewCell>> {
    if cell_w == 0 || cell_h == 0 {
        return Vec::new();
    }
    let (target_w, target_h) =
        compute_halfblock_dimensions(img.width(), img.height(), cell_w, cell_h);
    if target_w == 0 || target_h == 0 {
        return Vec::new();
    }

    let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
    let rgba = resized.to_rgba8();

    let cell_rows = target_h.div_ceil(2) as usize;
    let cell_cols = target_w as usize;

    let mut grid = Vec::with_capacity(cell_rows);
    for cy in 0..cell_rows {
        let mut row = Vec::with_capacity(cell_cols);
        let py_top = (cy * 2) as u32;
        let py_bot = py_top + 1;
        for cx in 0..cell_cols {
            let top_p = rgba.get_pixel(cx as u32, py_top);
            let bot_p = if py_bot < target_h {
                rgba.get_pixel(cx as u32, py_bot)
            } else {
                &image::Rgba([0, 0, 0, 255])
            };
            row.push(PreviewCell {
                top: blend_rgba(top_p),
                bottom: blend_rgba(bot_p),
            });
        }
        grid.push(row);
    }

    grid
}

/// Alpha blend RGBA pixel over black background into RGB.
fn blend_rgba(p: &image::Rgba<u8>) -> (u8, u8, u8) {
    let a = p[3] as u32;
    let r = (p[0] as u32 * a / 255) as u8;
    let g = (p[1] as u32 * a / 255) as u8;
    let b = (p[2] as u32 * a / 255) as u8;
    (r, g, b)
}
