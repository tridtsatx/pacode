use std::io::Write;
use tempfile::NamedTempFile;

use super::*;

fn create_test_png(w: u32, h: u32) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    let img = image::RgbaImage::new(w, h);
    let dynamic = image::DynamicImage::ImageRgba8(img);
    dynamic
        .write_to(&mut file, image::ImageFormat::Png)
        .unwrap();
    file.flush().unwrap();
    file
}

#[test]
fn test_render_halfblocks_success() {
    let file = create_test_png(10, 10);
    let req = PreviewRequest {
        path: file.path().to_path_buf(),
        cell_w: 5,
        cell_h: 5,
    };

    let res = render(&req, GraphicsProtocol::HalfBlocks).unwrap();
    match res {
        Preview::Cells(cells) => {
            assert!(!cells.is_empty());
        }
        Preview::Escape(_) => panic!("Expected cells preview"),
    }
}

#[test]
fn test_render_sixel_falls_back_to_halfblocks() {
    let file = create_test_png(10, 10);
    let req = PreviewRequest {
        path: file.path().to_path_buf(),
        cell_w: 5,
        cell_h: 5,
    };

    let res = render(&req, GraphicsProtocol::Sixel).unwrap();
    match res {
        Preview::Cells(cells) => {
            assert!(!cells.is_empty());
        }
        Preview::Escape(_) => panic!("Expected cells preview for sixel fallback"),
    }
}

#[test]
fn test_render_kitty_success() {
    let file = create_test_png(10, 10);
    let req = PreviewRequest {
        path: file.path().to_path_buf(),
        cell_w: 5,
        cell_h: 5,
    };

    let res = render(&req, GraphicsProtocol::Kitty).unwrap();
    match res {
        Preview::Escape(esc) => {
            assert!(esc.starts_with("\x1b_Ga=T,f=32,s=10,v=10,c=5,r=5,"));
            assert!(esc.ends_with("\x1b\\"));
        }
        Preview::Cells(_) => panic!("Expected escape preview"),
    }
}

#[test]
fn test_render_iterm2_success() {
    let file = create_test_png(10, 10);
    let req = PreviewRequest {
        path: file.path().to_path_buf(),
        cell_w: 5,
        cell_h: 5,
    };

    let res = render(&req, GraphicsProtocol::Iterm2).unwrap();
    match res {
        Preview::Escape(esc) => {
            assert!(
                esc.starts_with("\x1b]1337;File=inline=1;width=5;height=5;preserveAspectRatio=1:")
            );
            assert!(esc.ends_with("\x07"));
        }
        Preview::Cells(_) => panic!("Expected escape preview"),
    }
}

#[test]
fn test_file_size_cap_exceeded() {
    let file = NamedTempFile::new().unwrap();
    // Sparse file of 16 MiB + 1 byte
    file.as_file().set_len(MAX_FILE_SIZE + 1).unwrap();

    let req = PreviewRequest {
        path: file.path().to_path_buf(),
        cell_w: 10,
        cell_h: 10,
    };

    let err = render(&req, GraphicsProtocol::HalfBlocks).unwrap_err();
    match err {
        ImageError::FileSizeExceeded { size, limit } => {
            assert_eq!(size, MAX_FILE_SIZE + 1);
            assert_eq!(limit, MAX_FILE_SIZE);
        }
        other => panic!("Expected FileSizeExceeded, got {other:?}"),
    }
}

#[test]
fn test_dimensions_cap_exceeded() {
    // A real, decodable PNG one pixel past the width cap: the dimension guard
    // must trip before any full-frame decode happens.
    let mut file = NamedTempFile::new().unwrap();
    let img = image::RgbImage::new(8001, 1);
    let mut bytes: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    file.write_all(&bytes).unwrap();
    file.flush().unwrap();

    let req = PreviewRequest {
        path: file.path().to_path_buf(),
        cell_w: 10,
        cell_h: 10,
    };

    let err = render(&req, GraphicsProtocol::HalfBlocks).unwrap_err();
    match err {
        ImageError::DimensionsExceeded {
            width,
            height,
            max_width,
            max_height,
        } => {
            assert_eq!(width, 8001);
            assert_eq!(height, 1);
            assert_eq!(max_width, 8000);
            assert_eq!(max_height, 8000);
        }
        other => panic!("Expected DimensionsExceeded, got {other:?}"),
    }
}
