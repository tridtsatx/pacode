use pacode_image::{InlineImage, PreviewCell};

use super::*;

fn image(cols: u16, rows: u16) -> InlineImage {
    InlineImage {
        cols,
        rows,
        cells: vec![vec![PreviewCell::default(); cols as usize]; rows as usize],
    }
}

fn key(hash: &str, cols: u16, rows: u16) -> ImageKey {
    ImageKey {
        hash: hash.to_string(),
        cols,
        rows,
    }
}

#[test]
fn a_miss_records_one_want_and_decodes_nothing() {
    let mut cache = ImageCellCache::default();
    for _ in 0..3 {
        assert!(matches!(cache.lookup("h", 10, 5), Lookup::Pending));
    }
    assert_eq!(cache.take_wants(), vec![key("h", 10, 5)]);
    assert!(cache.take_wants().is_empty(), "taking drains");
    assert!(cache.is_empty());
}

#[test]
fn a_hit_is_served_without_a_want() {
    let mut cache = ImageCellCache::default();
    cache.insert(key("h", 10, 5), Ok(image(10, 5)));
    for _ in 0..3 {
        assert!(matches!(cache.lookup("h", 10, 5), Lookup::Ready(_)));
    }
    assert!(cache.take_wants().is_empty());
    assert_eq!(cache.len(), 1);
}

#[test]
fn different_width_is_a_different_entry() {
    let mut cache = ImageCellCache::default();
    cache.insert(key("h", 10, 5), Ok(image(10, 5)));
    assert!(matches!(cache.lookup("h", 20, 5), Lookup::Pending));
    cache.insert(key("h", 20, 5), Ok(image(20, 5)));
    assert_eq!(cache.len(), 2);
}

#[test]
fn wants_are_bounded() {
    let mut cache = ImageCellCache::default();
    for i in 0..MAX_WANTS * 2 {
        cache.lookup(&format!("h{i}"), 4, 2);
    }
    assert_eq!(cache.take_wants().len(), MAX_WANTS);
}

#[test]
fn entry_cap_evicts_least_recently_used() {
    let mut cache = ImageCellCache::default();
    for i in 0..MAX_ENTRIES {
        cache.insert(key(&format!("h{i}"), 4, 2), Ok(image(4, 2)));
    }
    // Touch h0 so h1 becomes the oldest.
    assert!(matches!(cache.lookup("h0", 4, 2), Lookup::Ready(_)));
    cache.insert(key("new", 4, 2), Ok(image(4, 2)));
    assert_eq!(cache.len(), MAX_ENTRIES);
    assert!(
        matches!(cache.lookup("h1", 4, 2), Lookup::Pending),
        "h1 should have been evicted"
    );
    assert!(
        matches!(cache.lookup("h0", 4, 2), Lookup::Ready(_)),
        "h0 was recently used"
    );
}

#[test]
fn byte_cap_is_enforced() {
    let mut cache = ImageCellCache::default();
    // 400 x 400 cells x 6 bytes = 960 KB each: five exceed 4 MB.
    for i in 0..8 {
        cache.insert(key(&format!("big{i}"), 400, 400), Ok(image(400, 400)));
    }
    assert!(cache.bytes() <= MAX_BYTES, "{}", cache.bytes());
    assert!(cache.len() < 8);
}

#[test]
fn failures_are_kept_so_they_are_not_retried_and_clear_resets() {
    let mut cache = ImageCellCache::default();
    cache.lookup("x", 4, 2);
    cache.insert(key("x", 4, 2), Err("gone".into()));
    assert!(
        cache.take_wants().is_empty(),
        "the finished want is dropped"
    );
    assert!(matches!(cache.lookup("x", 4, 2), Lookup::Failed(err) if err == "gone"));
    assert!(
        cache.take_wants().is_empty(),
        "a failure asks for no decode"
    );
    assert_eq!(cache.bytes(), 0);
    cache.insert(key("y", 4, 2), Ok(image(4, 2)));
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.bytes(), 0);
}

#[test]
fn jobs_are_claimed_once_and_bounded() {
    let mut jobs = InlineImageJobs::default();
    assert!(jobs.start(&key("a", 4, 2)));
    assert!(!jobs.start(&key("a", 4, 2)), "already running");
    for i in 1..MAX_IN_FLIGHT {
        assert!(jobs.start(&key(&format!("b{i}"), 4, 2)));
    }
    assert!(!jobs.start(&key("over", 4, 2)), "pool full");
    assert!(jobs.finish(&key("a", 4, 2)));
    assert!(!jobs.finish(&key("a", 4, 2)));
    assert!(jobs.start(&key("over", 4, 2)));
}
