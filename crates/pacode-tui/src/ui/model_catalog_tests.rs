use pacode_types::{Effort, ModelInfo, ModelRoute, Pricing};

use super::*;

fn model(efforts: Option<&[Effort]>) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new("p", "m"),
        display_name: "m".to_string(),
        context_window: Some(200_000),
        supports_reasoning: true,
        supports_vision: false,
        supported_efforts: efforts.map(<[Effort]>::to_vec),
        pricing: None,
        ..Default::default()
    }
}

#[test]
fn cycling_forward_and_backward_skips_unaccepted_levels() {
    // medium and xhigh are missing, so `low` steps straight to `high`.
    let m = model(Some(&[Effort::Low, Effort::High, Effort::Max]));
    assert_eq!(next_effort(&m, Effort::Low), Some(Effort::High));
    assert_eq!(next_effort(&m, Effort::High), Some(Effort::Max));
    assert_eq!(prev_effort(&m, Effort::Max), Some(Effort::High));
    assert_eq!(prev_effort(&m, Effort::High), Some(Effort::Low));
}

/// Past the last accepted level the cycle comes back to the first *accepted*
/// one, not to the first rung of the ladder.
#[test]
fn cycling_wraps_to_the_first_accepted_level_not_the_first_rung() {
    let m = model(Some(&[Effort::Medium, Effort::Max]));
    assert_eq!(next_effort(&m, Effort::Max), Some(Effort::Medium));
    assert_eq!(prev_effort(&m, Effort::Medium), Some(Effort::Max));
}

/// `None` means the catalog enumerated nothing: every rung stays selectable.
#[test]
fn an_unknown_ladder_keeps_every_level() {
    let m = model(None);
    assert_eq!(accepted_efforts(&m), Effort::ALL.to_vec());
    assert_eq!(next_effort(&m, Effort::XHigh), Some(Effort::Max));
    assert_eq!(next_effort(&m, Effort::Max), Some(Effort::Low));
}

#[test]
fn a_model_that_accepts_nothing_cannot_cycle() {
    let m = model(Some(&[]));
    assert_eq!(next_effort(&m, Effort::Medium), None);
    assert_eq!(prev_effort(&m, Effort::Medium), None);
    assert_eq!(snap_effort(&m, Effort::Medium), None);
    assert!(accepted_efforts(&m).is_empty());
}

#[test]
fn snapping_prefers_the_nearest_level_below_then_the_lowest() {
    let m = model(Some(&[Effort::Low, Effort::High]));
    assert_eq!(snap_effort(&m, Effort::High), Some(Effort::High));
    assert_eq!(snap_effort(&m, Effort::Max), Some(Effort::High));
    assert_eq!(snap_effort(&m, Effort::Medium), Some(Effort::Low));
    let m = model(Some(&[Effort::High, Effort::Max]));
    assert_eq!(snap_effort(&m, Effort::Low), Some(Effort::High));
}

#[test]
fn fuzzy_matches_subsequences_and_ranks_runs_and_word_starts() {
    assert_eq!(fuzzy_score("xyz", "claude sonnet"), None);
    assert!(fuzzy_score("", "anything").is_some());
    // Letters that navigate elsewhere are plain text here.
    assert!(fuzzy_score("hjkl", "haiku jk lite").is_some());
    let run = fuzzy_score("son", "claude sonnet").unwrap_or(i32::MIN);
    let scattered = fuzzy_score("son", "sigma opus nano").unwrap_or(i32::MIN);
    assert!(
        run > scattered,
        "a contiguous word-start run beats a scatter"
    );
    let start = fuzzy_score("op", "opus").unwrap_or(i32::MIN);
    let middle = fuzzy_score("op", "zzoopp").unwrap_or(i32::MIN);
    assert!(start > middle);
    // Case and query whitespace do not matter.
    assert_eq!(
        fuzzy_score("Claude Son", "claude-sonnet"),
        fuzzy_score("claudeson", "claude-sonnet")
    );
}

#[test]
fn labels_are_short_and_never_guess() {
    assert_eq!(ctx_label(Some(200_000)), "200k");
    assert_eq!(ctx_label(Some(1_048_576)), "1M");
    assert_eq!(ctx_label(Some(512)), "512");
    assert_eq!(ctx_label(None), "—");
    let price = |input_per_m, output_per_m| {
        price_label(Some(Pricing {
            input_per_m,
            output_per_m,
            cache_read_per_m: None,
            cache_write_per_m: None,
        }))
    };
    assert_eq!(price(3.0, 15.0), "$3/$15");
    assert_eq!(price(0.25, 1.5), "$0.25/$1.5");
    assert_eq!(price(0.0, 0.0), "free");
    assert_eq!(price_label(None), "—");
    let mut m = model(Some(&[Effort::Low]));
    m.supports_vision = true;
    assert_eq!(capability_tags(&m), "RVE");
}

#[test]
fn the_kind_filter_cycles_and_keeps_by_capability() {
    assert_eq!(Kind::All.next(), Kind::Reasoning);
    assert_eq!(Kind::Vision.next(), Kind::All);
    let m = model(None);
    assert!(Kind::Reasoning.keeps(&m));
    assert!(!Kind::Vision.keeps(&m));
}
