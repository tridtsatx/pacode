use std::collections::HashMap;

use pacode_types::model::ModelInfo;
use pacode_types::model::{ModelFlags, ModelRoute};

use super::{picker_models, zdr_active};

fn mi(provider: &str, model: &str, display: &str) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider, model),
        display_name: display.to_string(),
        context_window: None,
        supports_reasoning: false,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    }
}

#[test]
fn picker_models_favorites_first_then_name() {
    let models = vec![
        mi("b", "m1", "Beta One"),
        mi("a", "m2", "Alpha Two"),
        mi("c", "m3", "Gamma Three"),
    ];
    let flags = HashMap::from([(
        "c/m3".to_string(),
        ModelFlags {
            zdr_capable: false,
            favorite: true,
        },
    )]);

    let rows = picker_models(&models, &flags, "", false);
    let routes: Vec<String> = rows.iter().map(|m| m.route.to_string()).collect();
    assert_eq!(routes, vec!["c/m3", "a/m2", "b/m1"]);
}

#[test]
fn picker_models_query_filters_route_and_display() {
    let models = vec![
        mi("anthropic", "claude-4", "Claude 4"),
        mi("openai", "gpt-5", "GPT-5"),
    ];
    let flags = HashMap::new();

    let rows = picker_models(&models, &flags, "gpt", false);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].route.to_string(), "openai/gpt-5");

    // Query against display_name also matches.
    let rows = picker_models(&models, &flags, "claude", false);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].route.to_string(), "anthropic/claude-4");
}

#[test]
fn picker_models_zdr_only_keeps_capable() {
    let models = vec![
        mi("a", "m1", "One"),
        mi("a", "m2", "Two"),
        mi("a", "m3", "Three"),
    ];
    let flags = HashMap::from([
        (
            "a/m1".to_string(),
            ModelFlags {
                zdr_capable: true,
                favorite: false,
            },
        ),
        (
            "a/m3".to_string(),
            ModelFlags {
                zdr_capable: true,
                favorite: true,
            },
        ),
    ]);

    let rows = picker_models(&models, &flags, "", true);
    let routes: Vec<String> = rows.iter().map(|m| m.route.to_string()).collect();
    // m2 dropped; favorite m3 still sorts before m1.
    assert_eq!(routes, vec!["a/m3", "a/m1"]);
}

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
