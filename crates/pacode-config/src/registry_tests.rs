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
    assert_eq!(cfg.exec.yield_after_secs, 10);

    apply_to_config(&mut cfg, "exec.yield_after_secs", &toml::Value::Integer(45));
    assert_eq!(cfg.exec.yield_after_secs, 45);

    reset_in_config(&mut cfg, "exec.yield_after_secs");
    assert_eq!(cfg.exec.yield_after_secs, 10);
}
