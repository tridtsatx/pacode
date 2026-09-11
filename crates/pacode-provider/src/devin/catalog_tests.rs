use pacode_types::Effort;

use super::{Family, effort_rank_from_display, family_display_name, fold_families, parse_model_id};

#[test]
fn effort_suffix_splits_off_the_family() {
    let p = parse_model_id("swe-2-max");
    assert_eq!(p.family, "swe-2");
    assert!(p.effort_rank.is_some());

    let p = parse_model_id("gemini-3-8-flash-low");
    assert_eq!(p.family, "gemini-3-8-flash");
}

#[test]
fn serving_variants_stay_in_the_family_id() {
    assert_eq!(
        parse_model_id("claude-opus-5-high-fast").family,
        "claude-opus-5-fast"
    );
    assert_eq!(
        parse_model_id("gpt-5-6-sol-none-priority").family,
        "gpt-5-6-sol-priority"
    );
    assert_eq!(parse_model_id("glm-5-2-max-1m").family, "glm-5-2-1m");
}

#[test]
fn an_id_without_an_effort_suffix_is_its_own_family() {
    let p = parse_model_id("swe-1-6");
    assert_eq!(p.family, "swe-1-6");
    assert_eq!(p.effort_rank, None);

    // Legacy ids carry no dashes to split on at all.
    let p = parse_model_id("MODEL_PRIVATE_2");
    assert_eq!(p.family, "MODEL_PRIVATE_2");
    assert_eq!(p.effort_rank, None);

    // "lightning" is part of the model name, not an effort.
    assert_eq!(
        parse_model_id("swe-1-7-lightning").family,
        "swe-1-7-lightning"
    );
    assert_eq!(
        parse_model_id("swe-1-7-lightning-medium").family,
        "swe-1-7-lightning"
    );
}

#[test]
fn display_name_supplies_the_effort_an_id_omits() {
    assert_eq!(
        effort_rank_from_display("SWE-1.7 Max"),
        parse_model_id("x-max").effort_rank
    );
    assert_eq!(
        effort_rank_from_display("GLM-5.2 High"),
        parse_model_id("x-high").effort_rank
    );
    assert_eq!(
        effort_rank_from_display("Inkling X-High"),
        parse_model_id("x-xhigh").effort_rank
    );
    assert_eq!(
        effort_rank_from_display("GPT-5.6 Sol No Thinking"),
        parse_model_id("x-none").effort_rank
    );
    assert_eq!(effort_rank_from_display("Claude Opus 4.6"), None);
}

#[test]
fn family_display_name_drops_the_effort_word() {
    assert_eq!(family_display_name("Claude Opus 5 Max"), "Claude Opus 5");
    assert_eq!(family_display_name("Inkling X-High"), "Inkling");
    assert_eq!(
        family_display_name("GPT-5.6 Sol No Thinking"),
        "GPT-5.6 Sol"
    );
    assert_eq!(family_display_name("SWE-2"), "SWE-2");
}

#[test]
fn folding_groups_efforts_and_keeps_first_seen_order() {
    let families = fold_families([
        ("swe-2-medium", "SWE-2 Medium"),
        ("claude-opus-5-max", "Claude Opus 5 Max"),
        ("swe-2-high", "SWE-2 High"),
        ("swe-2-max", "SWE-2 Max"),
        ("claude-opus-5-low", "Claude Opus 5 Low"),
    ]);

    let ids: Vec<_> = families.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["swe-2", "claude-opus-5"]);
    assert_eq!(families[0].members.len(), 3);
}

fn swe2() -> Family {
    fold_families([
        ("swe-2-medium", "SWE-2 Medium"),
        ("swe-2-high", "SWE-2 High"),
        ("swe-2-max", "SWE-2 Max"),
    ])
    .remove(0)
}

#[test]
fn resolve_picks_the_exact_effort_when_the_family_serves_it() {
    assert_eq!(swe2().resolve(Effort::High), Some("swe-2-high"));
    assert_eq!(swe2().resolve(Effort::Max), Some("swe-2-max"));
}

#[test]
fn resolve_falls_back_to_the_nearest_effort_preferring_the_stronger_one() {
    // No low tier: medium is one step away, and nothing is closer.
    assert_eq!(swe2().resolve(Effort::Low), Some("swe-2-medium"));
    // No xhigh tier: high and max are both one step away, max wins.
    assert_eq!(swe2().resolve(Effort::XHigh), Some("swe-2-max"));
}

#[test]
fn an_effortless_id_is_ranked_by_its_display_name() {
    // "swe-1-7" is SWE-1.7 at max effort, and only "swe-1-7-medium" spells its
    // effort in the id; both belong to one family.
    let family = fold_families([
        ("swe-1-7", "SWE-1.7 Max"),
        ("swe-1-7-medium", "SWE-1.7 Medium"),
    ])
    .remove(0);
    assert_eq!(family.id, "swe-1-7");
    assert_eq!(family.resolve(Effort::Max), Some("swe-1-7"));
    assert_eq!(family.resolve(Effort::Low), Some("swe-1-7-medium"));
}

#[test]
fn a_family_with_one_unranked_member_always_resolves_to_it() {
    let family = fold_families([("claude-opus-4-6", "Claude Opus 4.6")]).remove(0);
    assert_eq!(family.resolve(Effort::Low), Some("claude-opus-4-6"));
    assert_eq!(family.resolve(Effort::Max), Some("claude-opus-4-6"));
}
