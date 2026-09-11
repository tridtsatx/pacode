use pacode_types::Config;
use tempfile::TempDir;

use super::*;
use crate::clipboard_read::PastedImages;
use crate::state::AppState;

fn make_state() -> AppState {
    AppState::new(Config::default(), "1.0.0".to_string(), 80, 24)
}

fn state_with_one_image() -> (AppState, TempDir, usize, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let mut state = make_state();
    state.pasted_images = PastedImages::new_in(tmp.path().to_path_buf());
    let rec = state.pasted_images.create_image_file(b"png").unwrap();
    (state, tmp, rec.token, rec.path)
}

#[test]
fn test_resolve_rewrites_token_to_at_path_and_collects_source() {
    let tmp = TempDir::new().unwrap();
    let mut images = PastedImages::new_in(tmp.path().to_path_buf());
    let rec = images.create_image_file(b"png").unwrap();

    let text = format!("look at {} please", rec.token_label());
    let (out, sources) = resolve_image_tokens(&images, &text);

    assert_eq!(out, format!("look at @{} please", rec.path.display()));
    assert_eq!(sources.len(), 1);
    match &sources[0] {
        ImageSource::Path(p) => assert_eq!(p, &rec.path),
        other => panic!("expected ImageSource::Path, got {other:?}"),
    }
}

#[test]
fn test_resolve_leaves_unknown_or_malformed_tokens_literal() {
    let tmp = TempDir::new().unwrap();
    let images = PastedImages::new_in(tmp.path().to_path_buf());

    for text in [
        "see [Image 9] here".to_string(),  // no such record
        "see [Image ] here".to_string(),   // no digits
        "see [Image 1x] here".to_string(), // digits but no bracket
        "see [Image 1]x".to_string(),      // token with no record either way
    ] {
        let (out, sources) = resolve_image_tokens(&images, &text);
        assert_eq!(out, text);
        assert!(sources.is_empty());
    }
}

#[test]
fn test_resolve_orders_sources_by_first_appearance_and_dedupes() {
    let tmp = TempDir::new().unwrap();
    let mut images = PastedImages::new_in(tmp.path().to_path_buf());
    let rec1 = images.create_image_file(b"a").unwrap();
    let rec2 = images.create_image_file(b"b").unwrap();

    let text = format!(
        "{} and {} and {}",
        rec2.token_label(),
        rec1.token_label(),
        rec2.token_label()
    );
    let (out, sources) = resolve_image_tokens(&images, &text);

    assert_eq!(
        out,
        format!(
            "@{} and @{} and @{}",
            rec2.path.display(),
            rec1.path.display(),
            rec2.path.display()
        )
    );
    assert_eq!(sources.len(), 2);
    match (&sources[0], &sources[1]) {
        (ImageSource::Path(a), ImageSource::Path(b)) => {
            assert_eq!(a, &rec2.path);
            assert_eq!(b, &rec1.path);
        }
        _ => panic!("expected two ImageSource::Path"),
    }
}

#[test]
fn test_token_label_does_not_prefix_collide() {
    // `[Image 1]` must not match literal text `[Image 12]` — the closing
    // bracket is part of the needle.
    let tmp = TempDir::new().unwrap();
    let mut images = PastedImages::new_in(tmp.path().to_path_buf());
    let rec = images.create_image_file(b"png").unwrap();
    assert_eq!(rec.token_label(), "[Image 1]");

    let (out, sources) = resolve_image_tokens(&images, "look [Image 12] ok");
    assert_eq!(out, "look [Image 12] ok");
    assert!(sources.is_empty());
}

#[test]
fn test_live_pasted_images_and_strip_rows() {
    let (mut state, _tmp, token, _path) = state_with_one_image();
    state.input.text = format!("look [Image {token}] please");
    assert_eq!(state.live_pasted_images().len(), 1);
    assert_eq!(state.image_strip_rows(), STRIP_ROWS.min(STRIP_MAX_ROWS));

    // Token removed from input -> no live images, no strip.
    state.input.text = "look elsewhere".to_string();
    assert!(state.live_pasted_images().is_empty());
    assert_eq!(state.image_strip_rows(), 0);

    // Submitted record is not live even while its token lingers.
    state.input.text = format!("again [Image {token}]");
    state.pasted_images.mark_submitted(&state.input.text);
    assert!(state.live_pasted_images().is_empty());
    assert_eq!(state.image_strip_rows(), 0);
}

#[test]
fn test_strip_rows_zero_when_images_disabled() {
    let (mut state, _tmp, token, _path) = state_with_one_image();
    state.config.ui.images = "off".to_string();
    state.input.text = format!("look [Image {token}]");
    assert_eq!(state.image_strip_rows(), 0);
}

#[test]
fn test_strip_cache_retains_only_live_tokens() {
    let mut strip = ImageStripState::default();
    for token in [1usize, 2, 3] {
        strip.thumbs.insert(
            token,
            CachedThumb {
                cell_w: 10,
                cell_h: 3,
                preview: None,
            },
        );
    }
    strip.retain(&[2]);
    assert!(strip.thumbs.contains_key(&2));
    assert_eq!(strip.thumbs.len(), 1);
}
