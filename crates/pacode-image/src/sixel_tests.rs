use super::*;
use crate::GraphicsProtocol;

#[test]
fn test_encode_sixel_dcs_framing() {
    // 2x1 pixels: red, green.
    let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
    let esc = encode_sixel_rgba(&rgba, 2, 1).expect("encode failed");
    assert!(esc.starts_with("\x1bP"), "missing DCS header: {esc:?}");
    assert!(esc.contains('q'), "missing sixel introducer 'q': {esc:?}");
    assert!(esc.ends_with("\x1b\\"), "missing ST trailer: {esc:?}");
    // Non-empty payload between header and trailer.
    assert!(esc.len() > "\x1bPq\x1b\\".len());
}

#[test]
fn test_encode_sixel_rejects_bad_input() {
    // Zero dimensions are rejected.
    assert!(encode_sixel_rgba(&[], 0, 0).is_err());
    // Buffer smaller than width*height*4 is rejected.
    let rgba = [0u8; 4];
    assert!(encode_sixel_rgba(&rgba, 2, 1).is_err());
}

#[test]
fn test_render_sixel_produces_escape() {
    // A tiny PNG written to a temp file renders through the sixel path as an escape.
    let img = image::RgbaImage::from_raw(
        2,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ],
    )
    .expect("image buffer");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tiny.png");
    img.save(&path).expect("save png");

    let req = crate::PreviewRequest {
        path,
        cell_w: 10,
        cell_h: 5,
    };
    let preview = crate::render(&req, GraphicsProtocol::Sixel).expect("render");
    match preview {
        crate::Preview::Escape(esc) => {
            assert!(esc.starts_with("\x1bP"));
            assert!(esc.ends_with("\x1b\\"));
        }
        crate::Preview::Cells(_) => panic!("sixel must render to an escape sequence"),
    }
}
