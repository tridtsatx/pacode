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
    cfg.daemon.socket = Some(PathBuf::from("/tmp/pacode.sock"));
    cfg.context.compaction_model = Some("test/compact".to_string());
    cfg.session.title_model = Some("test/title".to_string());
    cfg.permissions.explain_model = Some("test/explain".to_string());
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
fn limits_settings_round_trip_and_validate() {
    let mut cfg = Config::default();
    assert!(validate_config(&cfg).is_ok());
    assert_eq!(
        read_value(&cfg, "limits.max_provider_concurrency"),
        Some("128".to_string())
    );

    let entry = find_entry("limits.max_provider_concurrency").unwrap();
    assert_eq!(entry.default_value, "128");
    assert_eq!(entry.apply, ApplyMode::Restart);
    assert_eq!(entry.kind, SettingKind::Integer { min: 1, max: 1024 });

    let parsed = validate_candidate(entry, "512").unwrap();
    apply_to_config(&mut cfg, "limits.max_provider_concurrency", &parsed);
    assert_eq!(cfg.limits.max_provider_concurrency, 512);
    assert!(validate_config(&cfg).is_ok());

    // The runtime rejects 0 and anything above its own ceiling, so the registry
    // refuses them before the value can reach it.
    assert!(validate_candidate(entry, "0").is_err());
    assert!(validate_candidate(entry, "1024").is_ok());
    assert!(validate_candidate(entry, "1025").is_err());
}

#[test]
fn dead_limits_are_gone_from_the_registry() {
    // Each of these was configurable and read by nothing; a setting row would
    // promise a cap the runtime does not enforce.
    for key in [
        "limits.max_logical_agents",
        "limits.max_provider_queue_items",
        "limits.max_provider_queue_bytes",
        "limits.max_exec_calls_per_turn",
        "limits.max_tool_calls_per_turn",
    ] {
        assert!(find_entry(key).is_none(), "{key} must not be a setting");
        assert!(validate(key, "1").is_err(), "{key} must be unknown");
    }
    let cfg = Config::default();
    for key in [
        "limits.max_logical_agents",
        "limits.max_provider_queue_items",
        "limits.max_provider_queue_bytes",
        "limits.max_exec_calls_per_turn",
        "limits.max_tool_calls_per_turn",
    ] {
        assert!(read_value(&cfg, key).is_none(), "{key} must not read back");
    }
}

#[test]
fn cross_field_rules_flag_and_repair() {
    // A tool result bigger than the window can never be sent.
    let mut cfg = Config::default();
    cfg.context.tool_output_cap_chars = 500_000;
    let error = validate_config(&cfg).expect_err("tool cap cannot exceed the window");
    assert_eq!(error.key, "context.tool_output_cap_chars");
    assert!(
        error
            .message
            .contains("context.default_context_window=128000")
    );

    let repairs = repair_config(&mut cfg);
    assert_eq!(repairs.len(), 1);
    assert_eq!(repairs[0].key, "context.tool_output_cap_chars");
    assert_eq!(cfg.context.tool_output_cap_chars, 128_000);
    assert!(validate_config(&cfg).is_ok());

    // An MCP sampling budget above the window fails at the provider.
    let mut cfg = Config::default();
    cfg.context.default_context_window = 4_000;
    cfg.context.tool_output_cap_chars = 2_000;
    cfg.mcp.sampling_max_tokens = 9_000;
    assert_eq!(
        validate_config(&cfg)
            .expect_err("sampling cannot exceed the window")
            .key,
        "mcp.sampling_max_tokens"
    );
    repair_config(&mut cfg);
    assert_eq!(cfg.mcp.sampling_max_tokens, 4_000);
    assert!(validate_config(&cfg).is_ok());

    // Two rules broken at once are both repaired in one pass.
    let mut cfg = Config::default();
    cfg.context.default_context_window = 1_000;
    cfg.context.tool_output_cap_chars = 9_000;
    cfg.mcp.sampling_max_tokens = 9_000;
    let repairs = repair_config(&mut cfg);
    assert_eq!(repairs.len(), 2, "{repairs:?}");
    assert_eq!(cfg.context.tool_output_cap_chars, 1_000);
    assert_eq!(cfg.mcp.sampling_max_tokens, 1_000);
    assert!(validate_config(&cfg).is_ok());

    // A tail above the spool cap is RAM nothing can fill.
    let mut cfg = Config::default();
    cfg.exec.max_spool_bytes = 4_096;
    cfg.exec.tail_bytes = 65_536;
    assert_eq!(
        validate_config(&cfg)
            .expect_err("tail cannot exceed the spool")
            .key,
        "exec.tail_bytes"
    );
    repair_config(&mut cfg);
    assert_eq!(cfg.exec.tail_bytes, 4_096);
    assert!(validate_config(&cfg).is_ok());

    // Defaults hold every rule, and repairing them changes nothing.
    let mut cfg = Config::default();
    assert!(cross_field_violation(&cfg).is_none());
    assert!(repair_config(&mut cfg).is_empty());
}

#[test]
fn repair_config_clamps_out_of_range_values_instead_of_failing() {
    let mut cfg = Config::default();
    // Values an older pacode accepted: a wider provider ceiling than the
    // runtime has, a `max_retries` above the registry range, an `ups` past the
    // frame clock's range, a colour that is no longer a colour, and an `ups`
    // that is not a number at all.
    cfg.limits.max_provider_concurrency = 4_096;
    cfg.provider.max_retries = 101;
    cfg.ui.ups = pacode_types::Ups::Fixed(900);
    cfg.ui.color = "sepia".to_string();
    cfg.ui.animation = pacode_types::config::AnimationMode::Eco;
    cfg.mcp.sampling_max_tokens = 5_000_000;
    cfg.context.default_context_window = 2_000;

    let repairs = repair_config(&mut cfg);
    let keys: Vec<&str> = repairs.iter().map(|r| r.key).collect();
    assert!(
        keys.contains(&"limits.max_provider_concurrency"),
        "{keys:?}"
    );
    assert!(keys.contains(&"provider.max_retries"), "{keys:?}");
    assert!(keys.contains(&"ui.ups"), "{keys:?}");
    assert!(keys.contains(&"ui.color"), "{keys:?}");
    assert!(keys.contains(&"mcp.sampling_max_tokens"), "{keys:?}");

    assert_eq!(cfg.limits.max_provider_concurrency, 1_024);
    assert_eq!(cfg.provider.max_retries, 100);
    assert_eq!(cfg.ui.ups, pacode_types::Ups::Fixed(240));
    assert_eq!(cfg.ui.color, "auto");
    assert_eq!(cfg.mcp.sampling_max_tokens, 2_000);
    // A value inside its own range is left alone.
    assert_eq!(cfg.ui.animation, pacode_types::config::AnimationMode::Eco);
    assert!(validate_config(&cfg).is_ok());

    // A `ui.ups` that is not a number at all falls back to the default.
    let mut cfg = Config::default();
    cfg.ui.ups = pacode_types::Ups::Dynamic;
    cfg.ui.color = "sepia".to_string();
    let repairs = repair_config(&mut cfg);
    assert!(repairs.iter().any(|r| r.key == "ui.color"));
    assert_eq!(cfg.ui.ups, pacode_types::Ups::Dynamic);
    assert_eq!(cfg.ui.color, "auto");
}

#[test]
fn deprecated_agent_keys_are_not_active_registry_settings() {
    assert!(find_entry("agents.default_model").is_none());
    assert!(find_entry("agents.default_effort").is_none());
    assert!(!SECTIONS.contains(&"agents"));
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
fn test_permissions_explain_model_round_trip() {
    let mut cfg = Config::default();
    assert_eq!(
        read_value(&cfg, "permissions.explain_model"),
        Some("".to_string())
    );

    let entry = find_entry("permissions.explain_model").unwrap();
    assert_eq!(entry.default_value, "");
    assert_eq!(entry.kind, SettingKind::String);

    apply_to_config(
        &mut cfg,
        "permissions.explain_model",
        &toml::Value::String("anthropic/claude-haiku".to_string()),
    );
    assert_eq!(
        cfg.permissions.explain_model.as_deref(),
        Some("anthropic/claude-haiku")
    );
    assert_eq!(
        read_value(&cfg, "permissions.explain_model"),
        Some("anthropic/claude-haiku".to_string())
    );

    // Empty string resets to `None` — the session model is used again.
    apply_to_config(
        &mut cfg,
        "permissions.explain_model",
        &toml::Value::String(String::new()),
    );
    assert_eq!(cfg.permissions.explain_model, None);
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
            ApplyMode::RestartClient => assert_eq!(entry.apply.as_str(), "restart client"),
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
    // The provider governor ceiling is set once, at daemon startup.
    assert_eq!(
        find_entry("limits.max_provider_concurrency").unwrap().apply,
        ApplyMode::Restart
    );
    // So is everything the MCP pool reads when it is built.
    assert_eq!(
        find_entry("mcp.sampling").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(
        find_entry("mcp.sampling_max_tokens").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(
        find_entry("mcp.idle_timeout_secs").unwrap().apply,
        ApplyMode::Restart
    );
    assert_eq!(
        find_entry("font.family").unwrap().apply,
        ApplyMode::RestartClient
    );
    assert_eq!(
        find_entry("font.size").unwrap().apply,
        ApplyMode::RestartClient
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
    assert_eq!(find_entry("ui.mouse").unwrap().apply, ApplyMode::Immediate);

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

/// `ui.powerline` defaults off, survives a TOML round trip, and is an
/// immediate bool the Settings overlay can read and write.
#[test]
fn ui_powerline_is_an_immediate_bool_that_round_trips() {
    let cfg = Config::default();
    assert!(!cfg.ui.powerline);

    let parsed: Config = toml::from_str("[ui]\npowerline = true\n").expect("parse");
    assert!(parsed.ui.powerline);
    let text = toml::to_string(&parsed).expect("serialize");
    let back: Config = toml::from_str(&text).expect("reparse");
    assert!(back.ui.powerline);

    let entry = find_entry("ui.powerline").expect("registered");
    assert_eq!(entry.section, "ui");
    assert_eq!(entry.default_value, "false");
    assert!(matches!(entry.kind, SettingKind::Bool));
    assert_eq!(entry.apply, ApplyMode::Immediate);

    let mut cfg = Config::default();
    assert_eq!(read_value(&cfg, "ui.powerline"), Some("false".to_string()));
    apply_to_config(&mut cfg, "ui.powerline", &toml::Value::Boolean(true));
    assert!(cfg.ui.powerline);
    assert_eq!(read_value(&cfg, "ui.powerline"), Some("true".to_string()));
    reset_in_config(&mut cfg, "ui.powerline");
    assert!(!cfg.ui.powerline);
}

#[test]
fn plan_reminder_settings_round_trip() {
    let parsed: Config =
        toml::from_str("[session]\nplan_reminders = false\nplan_reminders_max = 5\n")
            .expect("parse");
    assert!(!parsed.session.plan_reminders);
    assert_eq!(parsed.session.plan_reminders_max, 5);
    let text = toml::to_string(&parsed).expect("serialize");
    let back: Config = toml::from_str(&text).expect("reparse");
    assert_eq!(back.session, parsed.session);

    let enabled = find_entry("session.plan_reminders").expect("registered");
    assert_eq!(enabled.section, "session");
    assert_eq!(enabled.default_value, "true");
    assert!(matches!(enabled.kind, SettingKind::Bool));
    let max = find_entry("session.plan_reminders_max").expect("registered");
    assert_eq!(max.default_value, "3");
    assert!(matches!(max.kind, SettingKind::Integer { min: 1, max: 10 }));
    assert!(validate_candidate(max, "0").is_err());

    let mut cfg = Config::default();
    assert_eq!(
        read_value(&cfg, "session.plan_reminders"),
        Some("true".into())
    );
    assert_eq!(
        read_value(&cfg, "session.plan_reminders_max"),
        Some("3".into())
    );
    apply_to_config(
        &mut cfg,
        "session.plan_reminders",
        &toml::Value::Boolean(false),
    );
    apply_to_config(
        &mut cfg,
        "session.plan_reminders_max",
        &toml::Value::Integer(7),
    );
    assert!(!cfg.session.plan_reminders);
    assert_eq!(cfg.session.plan_reminders_max, 7);
    reset_in_config(&mut cfg, "session.plan_reminders");
    reset_in_config(&mut cfg, "session.plan_reminders_max");
    assert_eq!(cfg.session, Config::default().session);
}
