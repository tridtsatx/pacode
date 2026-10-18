use pacode_image::{InlineImage, PreviewCell};

use super::*;

fn image(cols: u16, rows: u16) -> InlineImage {
    InlineImage {
        cols,
        rows,
        cells: vec![vec![PreviewCell::default(); cols as usize]; rows as usize],
    }
}

#[test]
fn hit_does_not_render_again() {
    let mut cache = ImageCellCache::default();
    let mut renders = 0;
    for _ in 0..3 {
        cache
            .get_or_render::<()>("h", 10, 5, || {
                renders += 1;
                Ok(image(10, 5))
            })
            .unwrap();
    }
    assert_eq!(renders, 1);
    assert_eq!(cache.len(), 1);
}

#[test]
fn different_width_is_a_different_entry() {
    let mut cache = ImageCellCache::default();
    cache
        .get_or_render::<()>("h", 10, 5, || Ok(image(10, 5)))
        .unwrap();
    cache
        .get_or_render::<()>("h", 20, 5, || Ok(image(20, 5)))
        .unwrap();
    assert_eq!(cache.len(), 2);
}

#[test]
fn entry_cap_evicts_least_recently_used() {
    let mut cache = ImageCellCache::default();
    for i in 0..MAX_ENTRIES {
        cache
            .get_or_render::<()>(&format!("h{i}"), 4, 2, || Ok(image(4, 2)))
            .unwrap();
    }
    // Touch h0 so h1 becomes the oldest.
    cache
        .get_or_render::<()>("h0", 4, 2, || Ok(image(4, 2)))
        .unwrap();
    cache
        .get_or_render::<()>("new", 4, 2, || Ok(image(4, 2)))
        .unwrap();
    assert_eq!(cache.len(), MAX_ENTRIES);
    let mut rerendered = false;
    cache
        .get_or_render::<()>("h1", 4, 2, || {
            rerendered = true;
            Ok(image(4, 2))
        })
        .unwrap();
    assert!(rerendered, "h1 should have been evicted");
    let mut h0_rendered = false;
    cache
        .get_or_render::<()>("h0", 4, 2, || {
            h0_rendered = true;
            Ok(image(4, 2))
        })
        .unwrap();
    assert!(!h0_rendered, "h0 was recently used");
}

#[test]
fn byte_cap_is_enforced() {
    let mut cache = ImageCellCache::default();
    // 400 x 400 cells x 6 bytes = 960 KB each: five exceed 4 MB.
    for i in 0..8 {
        cache
            .get_or_render::<()>(&format!("big{i}"), 400, 400, || Ok(image(400, 400)))
            .unwrap();
    }
    assert!(cache.bytes() <= MAX_BYTES, "{}", cache.bytes());
    assert!(cache.len() < 8);
}

#[test]
fn failures_are_not_cached_and_clear_resets() {
    let mut cache = ImageCellCache::default();
    assert!(
        cache
            .get_or_render("x", 4, 2, || Err::<InlineImage, _>("gone"))
            .is_err()
    );
    assert!(cache.is_empty());
    cache
        .get_or_render::<()>("y", 4, 2, || Ok(image(4, 2)))
        .unwrap();
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.bytes(), 0);
}
