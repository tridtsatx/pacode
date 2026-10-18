use super::*;

fn png(w: u32, h: u32) -> Vec<u8> {
    let img =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([9, 99, 199])));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn wide_image_is_bounded_by_columns() {
    let out = render_inline(&png(800, 400), 40, 14).unwrap();
    assert_eq!(out.cols, 40);
    // 800x400 at 40 columns = 40x20 px = 10 cell rows.
    assert_eq!(out.rows, 10);
    assert!(out.cells.iter().all(|r| r.len() == 40));
}

#[test]
fn tall_image_is_bounded_by_14_rows() {
    let out = render_inline(&png(100, 1000), 80, 99).unwrap();
    assert_eq!(out.rows, INLINE_MAX_ROWS);
    assert!(out.cols <= 3, "cols={}", out.cols);
}

#[test]
fn undecodable_bytes_error() {
    assert!(render_inline(b"nope", 10, 10).is_err());
}

#[test]
fn cell_colours_come_from_the_image() {
    let out = render_inline(&png(10, 10), 10, 5).unwrap();
    assert_eq!(out.cells[0][0].top, (9, 99, 199));
    assert!(out.approx_bytes() > 0);
}
