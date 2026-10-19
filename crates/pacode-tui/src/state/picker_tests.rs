use pacode_types::model::ModelRoute;

use super::zdr_active;

#[test]
fn zdr_active_respects_mode() {
    use pacode_types::{Config, SessionMeta, ZdrMode};
    let mut config = Config::default();
    let meta = SessionMeta {
        id: pacode_types::SessionId::new("s1"),
        name: None,
        cwd: std::path::PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: ModelRoute::new("p", "m"),
        effort: pacode_types::Effort::Medium,
        mode: pacode_types::Mode::Build,
        first_prompt: None,
        zdr: true,
    };
    config.zdr = ZdrMode::Session;
    assert!(zdr_active(&config, Some(&meta)));
    assert!(!zdr_active(&config, None));
    config.zdr = ZdrMode::Always;
    assert!(zdr_active(&config, None));
    config.zdr = ZdrMode::Off;
    assert!(!zdr_active(&config, Some(&meta)));
}

#[test]
fn nearest_effort_picks_lower_supported() {
    use pacode_types::Effort;

    use super::nearest_supported_effort;

    // xhigh unsupported chain lands on the nearest lower entry.
    let s = [Effort::Low, Effort::Medium, Effort::Max];
    assert_eq!(
        nearest_supported_effort(Effort::XHigh, &s),
        Some(Effort::Medium)
    );
    assert_eq!(
        nearest_supported_effort(Effort::High, &[Effort::Low]),
        Some(Effort::Low)
    );
    // Nothing lower supported -> lowest supported overall.
    assert_eq!(
        nearest_supported_effort(Effort::Low, &[Effort::High, Effort::Max]),
        Some(Effort::High)
    );
    // Empty supported list -> no answer.
    assert_eq!(nearest_supported_effort(Effort::High, &[]), None);
}

#[test]
fn recents_from_prefs_are_deduplicated_and_capped() {
    let loaded = ["a", "b", "a", "c", "d", "e", "f", "g"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        super::pickers::capped_recents(loaded),
        vec!["a", "b", "c", "d", "e"]
    );
}
