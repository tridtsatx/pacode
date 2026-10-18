use std::time::Instant;

use pacode_render::RenderOptions;
use pacode_types::{AgentId, ImageRef, TranscriptItem, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::state::transcript::Transcript;

fn init_store() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    pacode_image::store::init(dir.path().to_path_buf());
}

fn stored_png(w: u32, h: u32) -> ImageRef {
    init_store();
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        w,
        h,
        image::Rgb([200, 50, 50]),
    ));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .expect("encode");
    let hash = pacode_image::store::put(&bytes, "image/png").expect("put");
    ImageRef {
        hash,
        mime: "image/png".into(),
        width: Some(w),
        height: Some(h),
        name: Some("/tmp/shot.png".into()),
    }
}

fn opts() -> RenderOptions {
    RenderOptions::new(80, false)
}

fn width_of(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| pacode_render::display_width(&s.content))
        .sum()
}

#[test]
fn square_image_is_14_rows_and_fits_the_width() {
    let image = stored_png(100, 100);
    let mut cache = ImageCellCache::default();
    let lines = render_images(&[image], 80, &opts(), &mut cache);
    // 14 image rows + the blank line after the picture.
    assert_eq!(lines.len(), 15);
    assert!(lines.last().unwrap().spans.is_empty());
    for line in &lines[..14] {
        assert!(width_of(line) <= 80);
        assert!(width_of(line) > INDENT.len());
    }
    // 100x100 px: 28 pixel rows tall -> 28 columns wide (square pixels).
    assert_eq!(width_of(&lines[0]), INDENT.len() + 28);
    assert_eq!(cache.len(), 1);
}

#[test]
fn wide_image_is_bounded_by_the_cell_width() {
    let image = stored_png(1000, 100);
    let mut cache = ImageCellCache::default();
    let lines = render_images(&[image], 40, &opts(), &mut cache);
    assert!(lines.iter().all(|l| width_of(l) <= 40));
    // 38 columns -> 38 px wide, 3.8 px tall -> 4 px -> 2 cell rows (+ blank).
    assert_eq!(lines.len(), 3);
}

#[test]
fn rows_are_painted_with_half_blocks_in_the_pixel_colours() {
    let image = stored_png(10, 10);
    let mut cache = ImageCellCache::default();
    let lines = render_images(&[image], 80, &opts(), &mut cache);
    let span = &lines[0].spans[1];
    assert!(span.content.starts_with('\u{2580}'));
    assert_eq!(span.style.fg, Some(Color::Rgb(200, 50, 50)));
    assert_eq!(span.style.bg, Some(Color::Rgb(200, 50, 50)));
    // Identical cells are merged into one span.
    assert_eq!(lines[0].spans.len(), 2);
}

#[test]
fn ansi_terminals_get_indexed_colours() {
    let image = stored_png(10, 10);
    let mut cache = ImageCellCache::default();
    cache.truecolor = false;
    let lines = render_images(&[image], 80, &opts(), &mut cache);
    assert!(!matches!(lines[0].spans[1].style.fg, Some(Color::Rgb(..))));
}

#[test]
fn off_draws_a_one_line_stub() {
    let image = stored_png(800, 600);
    let mut cache = ImageCellCache::default();
    cache.enabled = false;
    let lines = render_images(&[image], 80, &opts(), &mut cache);
    assert_eq!(lines.len(), 2);
    let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text.trim(), "[image: shot.png 800\u{d7}600]");
    assert!(cache.is_empty());
}

#[test]
fn missing_bytes_degrade_to_a_stub() {
    init_store();
    let image = ImageRef {
        hash: "d".repeat(64),
        mime: "image/png".into(),
        width: Some(5),
        height: Some(5),
        name: None,
    };
    let mut cache = ImageCellCache::default();
    let lines = render_images(&[image], 80, &opts(), &mut cache);
    let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("unavailable"), "{text}");
}

#[test]
fn transcript_cell_draws_the_image_below_the_text_at_width_80() {
    let image = stored_png(100, 100);
    let mut transcript = Transcript::new(50);
    let now = Instant::now();
    transcript.upsert(
        TranscriptItem {
            seq: 1,
            agent: AgentId::main(),
            ts_ms: 1,
            kind: TranscriptKind::User {
                text: "what is in this picture?".into(),
            },
        },
        now,
    );
    transcript.upsert(
        TranscriptItem {
            seq: 2,
            agent: AgentId::main(),
            ts_ms: 2,
            kind: TranscriptKind::Images {
                images: vec![image],
            },
        },
        now,
    );

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let render_opts = opts();
    terminal
        .draw(|frame| {
            let area = frame.area();
            crate::ui::dialog::draw(frame, area, &mut transcript, &render_opts, 0, 0);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..80)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    // Row 0: user text; row 1: blank; rows 2..16: the picture.
    assert!(row_text(0).contains("what is in this picture?"));
    assert!(row_text(1).trim().is_empty());
    let picture_rows = (2..24)
        .filter(|y| row_text(*y).contains('\u{2580}'))
        .count();
    assert_eq!(picture_rows, 14);
    // Every picture cell is non-empty and carries a colour.
    assert_eq!(buffer[(3, 2)].symbol(), "\u{2580}");
    assert_eq!(buffer[(3, 2)].fg, Color::Rgb(200, 50, 50));
    // Cached lines: a second draw reuses the decoded cells.
    assert_eq!(transcript.images.len(), 1);
}

#[test]
fn chips_in_input_lines_are_styled_separately() {
    let base = Style::default();
    let chip = Style::default().fg(Color::Cyan);
    let spans = style_chips("see [Image 1 \u{b7} 3 B] now", base, chip);
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[1].content, "[Image 1 \u{b7} 3 B]");
    assert_eq!(spans[1].style, chip);
    assert_eq!(style_chips("plain", base, chip).len(), 1);
}
