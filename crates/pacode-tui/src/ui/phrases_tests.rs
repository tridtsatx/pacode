use super::*;

#[test]
fn phrase_is_stable_within_a_day_and_changes_across_days() {
    let day = day_index(1_757_000_000_000);
    assert_eq!(phrase_of_the_day(day), phrase_of_the_day(day));
    assert_ne!(phrase_of_the_day(day), phrase_of_the_day(day + 1));
}

#[test]
fn day_index_counts_whole_days() {
    assert_eq!(day_index(0), 0);
    assert_eq!(day_index(86_399_999), 0);
    assert_eq!(day_index(86_400_000), 1);
}

#[test]
fn every_phrase_is_short_and_non_empty() {
    for p in PHRASES {
        assert!(!p.is_empty());
        assert!(pacode_render::display_width(p) <= 48, "too long: {p}");
    }
}

#[test]
fn the_pool_wraps_around() {
    let n = PHRASES.len() as u64;
    assert_eq!(phrase_of_the_day(0), phrase_of_the_day(n));
}
