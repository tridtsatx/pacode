use chrono::TimeZone;

use super::*;

#[test]
fn test_pacman_verbs_list() {
    assert_eq!(PACMAN_VERBS.len(), 12);
    assert!(PACMAN_VERBS.contains(&"Munched"));
    assert!(PACMAN_VERBS.contains(&"Chomped"));
    assert!(PACMAN_VERBS.contains(&"Nibbled"));
    assert!(PACMAN_VERBS.contains(&"Crunched"));
    assert!(PACMAN_VERBS.contains(&"Gobbled"));
    assert!(PACMAN_VERBS.contains(&"Devoured"));
    assert!(PACMAN_VERBS.contains(&"Snacked"));
    assert!(PACMAN_VERBS.contains(&"Feasted"));
    assert!(PACMAN_VERBS.contains(&"Dotted"));
    assert!(PACMAN_VERBS.contains(&"Waka-waka'd"));
    assert!(PACMAN_VERBS.contains(&"Chewed"));
    assert!(PACMAN_VERBS.contains(&"Gnawed"));
}

#[test]
fn test_pick_verb_returns_valid_verb() {
    for _ in 0..50 {
        let verb = pick_verb();
        assert!(PACMAN_VERBS.contains(&verb));
    }
}

#[test]
fn test_stats_line_with_deterministic() {
    let dt = Local
        .with_ymd_and_hms(2026, 9, 9, 14, 5, 0)
        .single()
        .expect("valid datetime");
    let line = stats_line_with("Munched", 3200, dt);
    assert_eq!(line, "Munched for 3.2s · done 2:05 PM");

    let dt_am = Local
        .with_ymd_and_hms(2026, 9, 9, 9, 30, 0)
        .single()
        .expect("valid datetime");
    let line_am = stats_line_with("Gobbled", 65_000, dt_am);
    assert_eq!(line_am, "Gobbled for 1m05s · done 9:30 AM");
}

#[test]
fn test_stats_line_random_has_pacman_verb() {
    let line = stats_line(1500);
    assert!(PACMAN_VERBS.iter().any(|&v| line.starts_with(v)));
    assert!(line.contains("for 1.5s · done "));
}
