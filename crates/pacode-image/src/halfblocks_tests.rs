use super::*;

#[test]
fn test_downscale_dimensions_aspect_ratio_landscape() {
    // 2:1 image into 40x20 cells (pixel canvas is 40 wide x 40 high)
    // Scale is limited by width: ratio = 40/100 = 0.4.
    // Result: 40 wide, 20 high in pixels.
    let (w, h) = compute_halfblock_dimensions(100, 50, 40, 20);
    assert_eq!(w, 40);
    assert_eq!(h, 20);
}

#[test]
fn test_downscale_dimensions_aspect_ratio_portrait() {
    // 1:2 image into 40x20 cells (pixel canvas is 40 wide x 40 high)
    // Scale is limited by height: ratio = 40/100 = 0.4.
    // Result: 20 wide, 40 high in pixels.
    let (w, h) = compute_halfblock_dimensions(50, 100, 40, 20);
    assert_eq!(w, 20);
    assert_eq!(h, 40);
}

#[test]
fn test_downscale_dimensions_square() {
    // 1:1 image into 40x20 cells (pixel canvas 40x40)
    let (w, h) = compute_halfblock_dimensions(100, 100, 40, 20);
    assert_eq!(w, 40);
    assert_eq!(h, 40);
}

#[test]
fn test_downscale_dimensions_zero() {
    assert_eq!(compute_halfblock_dimensions(0, 100, 40, 20), (0, 0));
    assert_eq!(compute_halfblock_dimensions(100, 0, 40, 20), (0, 0));
    assert_eq!(compute_halfblock_dimensions(100, 100, 0, 20), (0, 0));
    assert_eq!(compute_halfblock_dimensions(100, 100, 40, 0), (0, 0));
}

#[test]
fn test_render_halfblocks_grid() {
    // Create a 2x2 test image:
    // Top-left: red, Top-right: green
    // Bottom-left: blue, Bottom-right: white
    let mut img = image::RgbaImage::new(2, 2);
    img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
    img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
    img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));

    let dynamic = image::DynamicImage::ImageRgba8(img);
    // Render into cell_w=2, cell_h=1 (pixel canvas is 2x2)
    let grid = render_halfblocks(&dynamic, 2, 1);

    // Should produce 1 cell row, 2 cell columns
    assert_eq!(grid.len(), 1);
    assert_eq!(grid[0].len(), 2);

    // Left cell: top is red, bottom is blue
    assert_eq!(grid[0][0].top, (255, 0, 0));
    assert_eq!(grid[0][0].bottom, (0, 0, 255));

    // Right cell: top is green, bottom is white
    assert_eq!(grid[0][1].top, (0, 255, 0));
    assert_eq!(grid[0][1].bottom, (255, 255, 255));
}
