use pacode_types::Config;
use pacode_types::model::Effort;

use super::*;

fn walk_leaf_keys(prefix: &str, value: &toml::Value, out: &mut Vec<String>) {
    match value {
        toml::Value::Table(table) => {
            if table.is_empty() {
                if !prefix.is_empty()
                    && !SETTINGS
                        .iter()
                        .any(|e| e.dotted_key.starts_with(&format!("{prefix}.")))
                {
                    out.push(prefix.to_string());
                }
            } else {
                for (k, v) in table {
                    let next = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk_leaf_keys(&next, v, out);
                }
            }
        }
        toml::Value::Array(_)
        | toml::Value::String(_)
        | toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_) => {
            if !prefix.is_empty() {
                out.push(prefix.to_string());
            }
        }
    }
}

#[test]
fn test_drift_guard_default_config() {
    let cfg = Config::default();
    let toml_val = toml::Value::try_from(&cfg).expect("Config::default should serialize to toml");

    let mut leaf_keys = Vec::new();
    walk_leaf_keys("", &toml_val, &mut leaf_keys);
    assert!(!leaf_keys.is_empty(), "Leaf keys must not be empty");

    for key in &leaf_keys {
        let in_registry = find_entry(key).is_some();
        let in_skip_list = SKIP_LIST.contains(&key.as_str());
        assert!(
            in_registry || in_skip_list,
            "Drift guard failure: leaf key '{key}' from Config::default() is neither in the registry nor on the SKIP_LIST"
        );
    }
}

#[test]
fn test_drift_guard_fully_populated_config() {
    let mut cfg = Config::default();
    // Populate all Option fields to Some(...) so serde outputs their keys
    cfg.provider.default = Some("test/model".to_string());
    cfg.agents.default_model = Some("test/subagent".to_string());
    cfg.agents.default_effort = Some(Effort::High);
    cfg.daemon.socket = Some(PathBuf::from("/tmp/pacode.sock"));
    cfg.context.compaction_model = Some("test/compact".to_string());
    cfg.session.title_model = Some("test/title".to_string());
    cfg.font.family = Some("JetBrains Mono".to_string());
    cfg.font.size = Some(14);
    cfg.font.weight = Some("bold".to_string());

    let toml_val = toml::Value::try_from(&cfg).expect("populated Config should serialize to toml");

    let mut leaf_keys = Vec::new();
    walk_leaf_keys("", &toml_val, &mut leaf_keys);

    for key in &leaf_keys {
        let in_registry = find_entry(key).is_some();
        let in_skip_list = SKIP_LIST.contains(&key.as_str());
        assert!(
            in_registry || in_skip_list,
            "Drift guard failure: populated leaf key '{key}' is neither in the registry nor on the SKIP_LIST"
        );
    }
}

#[test]
fn test_value_read_back_for_each_kind() {
    let mut cfg = Config::default();
    cfg.ui.mouse = true;
    cfg.exec.yield_after_secs = 25;
    cfg.context.compaction_threshold = 0.75;
    cfg.ui.color = "truecolor".to_string();
    cfg.provider.effort = Effort::High;

    // Bool
    assert_eq!(read_value(&cfg, "ui.mouse"), Some("true".to_string()));
    // Integer
    assert_eq!(
        read_value(&cfg, "exec.yield_after_secs"),
        Some("25".to_string())
    );
    // Float
    assert_eq!(
        read_value(&cfg, "context.compaction_threshold"),
        Some("0.75".to_string())
    );
    // Enum
    assert_eq!(read_value(&cfg, "ui.color"), Some("truecolor".to_string()));
    assert_eq!(
        read_value(&cfg, "provider.effort"),
        Some("high".to_string())
    );
    // Action
    assert!(
        read_value(&cfg, "keys")
            .unwrap()
            .contains("custom bindings (/keys)")
    );
}

#[test]
fn test_validation_accepts_good_values() {
    let bool_entry = find_entry("ui.mouse").unwrap();
    assert_eq!(
        validate_candidate(bool_entry, "true").unwrap(),
        toml::Value::Boolean(true)
    );
    assert_eq!(
        validate_candidate(bool_entry, "false").unwrap(),
        toml::Value::Boolean(false)
    );

    let int_entry = find_entry("exec.yield_after_secs").unwrap();
    assert_eq!(
        validate_candidate(int_entry, "30").unwrap(),
        toml::Value::Integer(30)
    );

    let float_entry = find_entry("context.compaction_threshold").unwrap();
    assert_eq!(
        validate_candidate(float_entry, "0.80").unwrap(),
        toml::Value::Float(0.80)
    );

    let enum_entry = find_entry("ui.color").unwrap();
    assert_eq!(
        validate_candidate(enum_entry, "ansi").unwrap(),
        toml::Value::String("ansi".to_string())
    );

    let string_entry = find_entry("theme.name").unwrap();
    assert_eq!(
        validate_candidate(string_entry, "my-custom-theme").unwrap(),
        toml::Value::String("my-custom-theme".to_string())
    );
}

#[test]
fn test_validation_rejects_out_of_range() {
    let int_entry = find_entry("exec.yield_after_secs").unwrap();
    let err_low = validate_candidate(int_entry, "0").unwrap_err();
    assert_eq!(
        err_low,
        ValidationError::IntegerOutOfRange {
            value: 0,
            min: 1,
            max: 3600
        }
    );
    assert!(err_low.to_string().contains("out of range"));

    let err_high = validate_candidate(int_entry, "5000").unwrap_err();
    assert_eq!(
        err_high,
        ValidationError::IntegerOutOfRange {
            value: 5000,
            min: 1,
            max: 3600
        }
    );

    let float_entry = find_entry("context.compaction_threshold").unwrap();
    let err_float_low = validate_candidate(float_entry, "0.05").unwrap_err();
    assert!(matches!(
        err_float_low,
        ValidationError::FloatOutOfRange { .. }
    ));

    let err_float_high = validate_candidate(float_entry, "1.5").unwrap_err();
    assert!(matches!(
        err_float_high,
        ValidationError::FloatOutOfRange { .. }
    ));
}

#[test]
fn test_validation_rejects_wrong_type() {
    let bool_entry = find_entry("ui.mouse").unwrap();
    let err_bool = validate_candidate(bool_entry, "yes").unwrap_err();
    assert!(matches!(err_bool, ValidationError::InvalidBool { .. }));
    assert!(err_bool.to_string().contains("invalid boolean"));

    let int_entry = find_entry("exec.yield_after_secs").unwrap();
    let err_int = validate_candidate(int_entry, "abc").unwrap_err();
    assert!(matches!(err_int, ValidationError::InvalidInteger { .. }));
    assert!(err_int.to_string().contains("invalid integer"));

    let float_entry = find_entry("context.compaction_threshold").unwrap();
    let err_float = validate_candidate(float_entry, "xyz").unwrap_err();
    assert!(matches!(err_float, ValidationError::InvalidFloat { .. }));
    assert!(err_float.to_string().contains("invalid float"));
}

#[test]
fn test_validation_rejects_non_member_of_enum() {
    let enum_entry = find_entry("ui.color").unwrap();
    let err_enum = validate_candidate(enum_entry, "neon").unwrap_err();
    assert_eq!(
        err_enum,
        ValidationError::InvalidOption {
            value: "neon".to_string(),
            options: vec![
                "auto".to_string(),
                "truecolor".to_string(),
                "ansi".to_string()
            ],
        }
    );
    assert!(err_enum.to_string().contains("expected one of"));
}

#[test]
fn test_validation_ups() {
    let ups_entry = find_entry("ui.ups").unwrap();
    assert_eq!(
        validate_candidate(ups_entry, "auto").unwrap(),
        toml::Value::String("auto".to_string())
    );
    assert_eq!(
        validate_candidate(ups_entry, "dynamic").unwrap(),
        toml::Value::String("dynamic".to_string())
    );
    assert_eq!(
        validate_candidate(ups_entry, "60").unwrap(),
        toml::Value::Integer(60)
    );

    let err_low = validate_candidate(ups_entry, "0").unwrap_err();
    assert!(matches!(err_low, ValidationError::InvalidUps { .. }));

    let err_high = validate_candidate(ups_entry, "300").unwrap_err();
    assert!(matches!(err_high, ValidationError::InvalidUps { .. }));
}

#[test]
fn test_ui_animation_round_trip() {
    let mut cfg = Config::default();
    assert_eq!(read_value(&cfg, "ui.animation"), Some("smooth".to_string()));

    let entry = find_entry("ui.animation").unwrap();
    assert_eq!(entry.default_value, "smooth");
    // An enum, not free text: the three presets are the whole range, so the
    // settings menu offers them instead of asking the reader to type one.
    assert_eq!(
        entry.kind,
        SettingKind::Enum {
            options: &["smooth", "balanced", "eco"]
        }
    );
    assert_eq!(entry.apply, ApplyMode::Immediate);

    let parsed = validate_candidate(entry, "eco").unwrap();
    assert_eq!(parsed, toml::Value::String("eco".to_string()));
    assert!(validate_candidate(entry, "turbo").is_err());

    apply_to_config(&mut cfg, "ui.animation", &parsed);
    assert_eq!(cfg.ui.animation, pacode_types::AnimationMode::Eco);
    assert_eq!(read_value(&cfg, "ui.animation"), Some("eco".to_string()));

    apply_to_config(
        &mut cfg,
        "ui.animation",
        &toml::Value::String("balanced".to_string()),
    );
    assert_eq!(cfg.ui.animation, pacode_types::AnimationMode::Balanced);

    reset_in_config(&mut cfg, "ui.animation");
    assert_eq!(cfg.ui.animation, pacode_types::AnimationMode::Smooth);
    assert_eq!(read_value(&cfg, "ui.animation"), Some("smooth".to_string()));
}

#[test]
fn test_action_setting_not_directly_editable() {
    let keys_entry = find_entry("keys").unwrap();
    let err = validate_candidate(keys_entry, "ctrl+c").unwrap_err();
    assert_eq!(
        err,
        ValidationError::NotDirectlyEditable {
            key: "keys".to_string(),
            command: "/keys".to_string(),
        }
    );
    assert!(err.to_string().contains("use /keys"));
}

#[test]
fn test_apply_and_reset_in_config() {
    let mut cfg = Config::default();
    assert_eq!(cfg.exec.yield_after_secs, 5);

    apply_to_config(&mut cfg, "exec.yield_after_secs", &toml::Value::Integer(45));
    assert_eq!(cfg.exec.yield_after_secs, 45);

    reset_in_config(&mut cfg, "exec.yield_after_secs");
    assert_eq!(cfg.exec.yield_after_secs, 5);
}

#[test]
fn test_catalog_ttl_secs_round_trip() {
    let mut cfg = Config::default();
    assert_eq!(cfg.provider.catalog_ttl_secs, 86400);
    assert_eq!(
        read_value(&cfg, "provider.catalog_ttl_secs"),
        Some("86400".to_string())
    );

    let entry = find_entry("provider.catalog_ttl_secs").unwrap();
    assert_eq!(entry.default_value, "86400");
    assert_eq!(entry.apply, ApplyMode::NextTurn);

    let parsed = validate_candidate(entry, "3600").unwrap();
    assert_eq!(parsed, toml::Value::Integer(3600));

    apply_to_config(&mut cfg, "provider.catalog_ttl_secs", &parsed);
    assert_eq!(cfg.provider.catalog_ttl_secs, 3600);
    assert_eq!(
        read_value(&cfg, "provider.catalog_ttl_secs"),
        Some("3600".to_string())
    );

    reset_in_config(&mut cfg, "provider.catalog_ttl_secs");
    assert_eq!(cfg.provider.catalog_ttl_secs, 86400);
    assert_eq!(
        read_value(&cfg, "provider.catalog_ttl_secs"),
        Some("86400".to_string())
    );

    // Rejects out of range (e.g. 0 or too high)
    assert!(validate_candidate(entry, "0").is_err());
    assert!(validate_candidate(entry, "3000000").is_err());
    assert!(validate_candidate(entry, "not-a-number").is_err());
}

#[test]
fn test_web_config_settings() {
    let mut cfg = Config::default();
    assert_eq!(
        read_value(&cfg, "web.default_num_results"),
        Some("8".to_string())
    );
    assert_eq!(
        read_value(&cfg, "web.request_timeout_secs"),
        Some("15".to_string())
    );

    apply_to_config(
        &mut cfg,
        "web.default_num_results",
        &toml::Value::Integer(12),
    );
    assert_eq!(cfg.web.default_num_results, 12);

    apply_to_config(
        &mut cfg,
        "web.request_timeout_secs",
        &toml::Value::Integer(30),
    );
    assert_eq!(cfg.web.request_timeout_secs, 30);

    reset_in_config(&mut cfg, "web.default_num_results");
    assert_eq!(cfg.web.default_num_results, 8);
}

#[test]
fn test_tool_toggles() {
    let mut cfg = Config::default();
    // Static entries exist for built-in tools.
    let entry = find_entry("tools.bash.enabled").unwrap();
    assert_eq!(entry.section, "tools");
    assert_eq!(entry.kind, SettingKind::Bool);
    assert_eq!(entry.apply, ApplyMode::Restart);

    // read_value: everything is enabled by default.
    assert_eq!(
        read_value(&cfg, "tools.bash.enabled"),
        Some("true".to_string())
    );

    // validate/apply round-trip — static name and a dynamic MCP-style name.
    let parsed = validate("tools.bash.enabled", "false").unwrap();
    apply_to_config(&mut cfg, "tools.bash.enabled", &parsed);
    assert!(!cfg.tool_enabled("bash"));
    assert_eq!(
        read_value(&cfg, "tools.bash.enabled"),
        Some("false".to_string())
    );

    let parsed = validate("tools.github__get_pr.enabled", "false").unwrap();
    apply_to_config(&mut cfg, "tools.github__get_pr.enabled", &parsed);
    assert!(!cfg.tool_enabled("github__get_pr"));

    // Bad names/values fail closed.
    assert!(validate("tools.bash.enabled", "maybe").is_err());
    assert!(matches!(
        validate("tools.a.b.enabled", "false"),
        Err(ValidationError::UnknownKey { .. })
    ));
    assert!(find_entry("tools.nonexistent.enabled").is_none());

    // is_modified tracks the disabled state.
    assert!(is_modified(&cfg, find_entry("tools.bash.enabled").unwrap()));
    assert!(!is_modified(
        &cfg,
        find_entry("tools.read.enabled").unwrap()
    ));
}

#[test]
fn test_live_restart_classification_present_for_every_entry() {
    for entry in SETTINGS {
        match entry.apply {
            ApplyMode::Immediate => assert_eq!(entry.apply.as_str(), "immediate"),
            ApplyMode::NextTurn => assert_eq!(entry.apply.as_str(), "next turn"),
            ApplyMode::Restart => assert_eq!(entry.apply.as_str(), "restart"),
        }
    }
}

#[test]
fn test_live_restart_classification_spot_checks() {
    // Restart: read once at startup / process spawn
    assert_eq!(
        find_entry("daemon.socket").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(
        find_entry("daemon.idle_timeout_secs").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(find_entry("mcp.servers").unwrap().apply, ApplyMode::Restart);
    assert_eq!(
        find_entry("plugins.enabled").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(
        find_entry("skills.enabled").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(
        find_entry("exec.stall_secs").unwrap().apply,
        ApplyMode::Restart
    );

    // Immediate: read on render / keypress / atomic update live
    assert_eq!(
        find_entry("ui.hints.model").unwrap().apply,
        ApplyMode::Immediate
    );
    assert_eq!(find_entry("ui.vim").unwrap().apply, ApplyMode::Immediate);
    assert_eq!(
        find_entry("theme.name").unwrap().apply,
        ApplyMode::Immediate
    );
    assert_eq!(
        find_entry("mcp.sampling").unwrap().apply,
        ApplyMode::Immediate
    );
    assert_eq!(
        find_entry("mcp.idle_timeout_secs").unwrap().apply,
        ApplyMode::Immediate
    );

    // NextTurn: read during turn execution / prompt building / agent spawning
    assert_eq!(
        find_entry("provider.default").unwrap().apply,
        ApplyMode::NextTurn
    );
    assert_eq!(
        find_entry("context.compaction_threshold").unwrap().apply,
        ApplyMode::NextTurn
    );
    assert_eq!(
        find_entry("agents.max_live").unwrap().apply,
        ApplyMode::NextTurn
    );
    assert_eq!(
        find_entry("permissions.default_mode").unwrap().apply,
        ApplyMode::NextTurn
    );
    assert_eq!(
        find_entry("exec.yield_after_secs").unwrap().apply,
        ApplyMode::NextTurn
    );
}

#[test]
fn value_origin_covers_default_global_and_env() {
    let cfg = Config::default();

    // Nothing overrides `ui.mouse` — the registry default is in effect.
    let mouse = find_entry("ui.mouse").unwrap();
    assert_eq!(value_origin(&cfg, mouse), Origin::Default);
    assert_eq!(value_origin(&cfg, mouse).as_str(), "default");

    // A value differing from the default came from the user config — the only
    // settings file pacode has.
    let mut overridden = cfg.clone();
    overridden.ui.mouse = false;
    assert_eq!(value_origin(&overridden, mouse), Origin::Global);

    // An env-overridable key reports Env the moment its var is set — even
    // when the file or the in-memory value agrees with the default.
    let effort = find_entry("provider.effort").unwrap();
    assert_eq!(
        value_origin_with(&cfg, effort, |var| var == "PACODE_EFFORT"),
        Origin::Env
    );
    // A var mapped to another key does not leak onto this one.
    assert_eq!(
        value_origin_with(&cfg, effort, |var| var == "PACODE_MODEL"),
        Origin::Default
    );
    // Env wins over a file-set value.
    let mut file_set = cfg.clone();
    file_set.provider.effort = Effort::Max;
    assert_eq!(
        value_origin_with(&file_set, effort, |var| var == "PACODE_EFFORT"),
        Origin::Env
    );

    // `Project` is reserved for a layer that does not exist yet — the variant
    // is part of the contract even though nothing returns it.
    assert_eq!(Origin::Project.as_str(), "project");
}
