use std::fs;
use std::os::unix::fs::PermissionsExt;

use pacode_types::{Config, Effort, Mode, ProviderConfig};
use tempfile::tempdir;

use crate::logging::{FileLogger, default_level, default_level_for, level_from_env};
use crate::paths::Paths;
use crate::{ConfigError, apply_env_overrides, load, parse, resolve_api_key};

const SPEC_EXAMPLE_TOML: &str = r#"
[provider]
default = "bubna/gemini-3.8-flash"
effort = "high"

[providers.bubna]
base_url = "https://api.example.com/v1"
api_key_env = "BUBNA_API_KEY"
catalog = true                   # GET /models
context_window = 131072
reasoning = true
# effort_map = { max = "xhigh" }
# extra_body = { chat_template_kwargs = { thinking = true } }

[pricing."gemini-3.8-flash"]
input_per_m = 0.30
output_per_m = 2.50

[ui]
ascii_only = false
mouse = true
[ui.hints]
model = true
effort = true

[exec]
yield_after_secs = 10
stall_secs = 120

[agents]
max_live = 8

[daemon]
idle_timeout_secs = 600

[context]
compaction_threshold = 0.85
tool_output_cap_chars = 16000

[permissions]
default_mode = "build"
allow_catastrophic = false

[mcp.servers.fs]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
lazy = true
"#;

#[test]
fn parse_spec_example() {
    let cfg = parse(SPEC_EXAMPLE_TOML).expect("failed to parse spec example");

    assert_eq!(
        cfg.provider.default.as_deref(),
        Some("bubna/gemini-3.8-flash")
    );
    assert_eq!(cfg.provider.effort, Effort::High);

    let bubna = cfg
        .providers
        .get("bubna")
        .expect("bubna provider must be present");
    assert_eq!(bubna.base_url, "https://api.example.com/v1");
    assert_eq!(bubna.api_key_env.as_deref(), Some("BUBNA_API_KEY"));
    assert!(bubna.catalog);
    assert_eq!(bubna.context_window, Some(131072));
    assert_eq!(bubna.reasoning, Some(true));

    let pricing = cfg
        .pricing
        .get("gemini-3.8-flash")
        .expect("gemini-3.8-flash pricing must be present");
    assert!((pricing.input_per_m - 0.30).abs() < 1e-6);
    assert!((pricing.output_per_m - 2.50).abs() < 1e-6);

    assert!(!cfg.ui.ascii_only);
    assert!(cfg.ui.mouse);
    assert_eq!(cfg.ui.ups, pacode_types::Ups::Fixed(10));
    assert!(cfg.ui.hints.model);
    assert!(cfg.ui.hints.effort);

    assert_eq!(cfg.exec.yield_after_secs, 10);
    assert_eq!(cfg.exec.stall_secs, 120);

    assert_eq!(cfg.agents.max_live, 8);

    assert_eq!(cfg.daemon.idle_timeout_secs, 600);

    assert!((cfg.context.compaction_threshold - 0.85).abs() < 1e-6);
    assert_eq!(cfg.context.tool_output_cap_chars, 16000);

    assert_eq!(cfg.permissions.default_mode, Mode::Build);
    assert!(!cfg.permissions.allow_catastrophic);

    let fs_server = cfg
        .mcp
        .servers
        .get("fs")
        .expect("mcp fs server must be present");
    assert_eq!(fs_server.command, "npx");
    assert_eq!(
        fs_server.args,
        vec!["-y", "@modelcontextprotocol/server-filesystem", "."]
    );
    assert!(fs_server.lazy);
    assert!(fs_server.enabled);
    assert_eq!(fs_server.url, None);
    assert!(fs_server.headers.is_empty());
    assert!(cfg.mcp.sampling);
    assert_eq!(cfg.mcp.sampling_max_tokens, 2048);
}

#[test]
fn defaults_from_empty_text() {
    let cfg = parse("").expect("empty toml should parse");
    let default = Config::default();
    assert_eq!(cfg, default);

    assert_eq!(cfg.provider.default, None);
    assert_eq!(cfg.provider.effort, Effort::Medium);
    assert_eq!(cfg.provider.stream_idle_secs, 180);
    assert_eq!(cfg.provider.max_retries, 5);

    assert_eq!(cfg.exec.yield_after_secs, 10);
    assert_eq!(cfg.exec.stall_secs, 120);
    assert_eq!(cfg.exec.max_spool_bytes, 50 * 1024 * 1024);
    assert_eq!(cfg.exec.tail_bytes, 64 * 1024);
    assert_eq!(cfg.exec.kill_grace_secs, 5);
    assert_eq!(cfg.exec.max_tasks, 64);
    assert_eq!(cfg.exec.default_timeout_secs, 3600);

    assert_eq!(cfg.agents.max_live, 8);
    assert_eq!(cfg.agents.max_turns, 200);

    assert_eq!(cfg.daemon.idle_timeout_secs, 600);

    assert!((cfg.context.compaction_threshold - 0.85).abs() < 1e-6);
    assert_eq!(cfg.context.tool_output_cap_chars, 16_000);
    assert_eq!(cfg.context.injection_cap_chars, 4_000);
    assert_eq!(cfg.context.instructions_cap_chars, 32_000);
    assert_eq!(cfg.context.keep_recent_messages, 6);
    assert_eq!(cfg.context.default_context_window, 128_000);

    assert_eq!(cfg.permissions.default_mode, Mode::Build);
    assert!(!cfg.permissions.allow_catastrophic);

    assert!(!cfg.ui.ascii_only);
    assert!(cfg.ui.mouse);
    assert_eq!(cfg.ui.ups, pacode_types::Ups::Fixed(10));
    assert_eq!(cfg.ui.transcript_cells, 500);
    assert!(!cfg.ui.hints.model);
    assert!(cfg.ui.hints.effort);
}

#[test]
fn env_overrides_valid() {
    let mut cfg = Config::default();
    apply_env_overrides(
        &mut cfg,
        [
            ("PACODE_MODEL", "custom/model-1"),
            ("PACODE_EFFORT", "max"),
            ("PACODE_MODE", "plan"),
            ("SOME_OTHER_VAR", "ignored"),
        ],
    )
    .expect("valid env overrides should succeed");

    assert_eq!(cfg.provider.default.as_deref(), Some("custom/model-1"));
    assert_eq!(cfg.provider.effort, Effort::Max);
    assert_eq!(cfg.permissions.default_mode, Mode::Plan);
}

#[test]
fn env_overrides_invalid_effort() {
    let mut cfg = Config::default();
    let err = apply_env_overrides(&mut cfg, [("PACODE_EFFORT", "ultra")])
        .expect_err("invalid effort must fail");

    match err {
        ConfigError::Env { var, value } => {
            assert_eq!(var, "PACODE_EFFORT");
            assert_eq!(value, "ultra");
        }
        other => panic!("expected ConfigError::Env, got: {other:?}"),
    }
}

#[test]
fn env_overrides_invalid_mode() {
    let mut cfg = Config::default();
    let err = apply_env_overrides(&mut cfg, [("PACODE_MODE", "godmode")])
        .expect_err("invalid mode must fail");

    match err {
        ConfigError::Env { var, value } => {
            assert_eq!(var, "PACODE_MODE");
            assert_eq!(value, "godmode");
        }
        other => panic!("expected ConfigError::Env, got: {other:?}"),
    }
}

#[test]
fn paths_under_layout() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let paths = Paths::under(root);

    assert_eq!(paths.config_file, root.join("config.toml"));
    assert_eq!(paths.data_dir, root.join("data"));
    assert_eq!(paths.state_dir, root.join("state"));
    assert_eq!(paths.cache_dir, root.join("cache"));
    assert_eq!(paths.runtime_dir, root.join("run"));

    assert_eq!(paths.db_file(), root.join("data/pacode.db"));
    assert_eq!(paths.daemon_log(), root.join("state/daemon.log"));
    assert_eq!(paths.client_log(), root.join("state/client.log"));
    assert_eq!(paths.spool_dir(), root.join("state/tasks"));
    assert_eq!(paths.tool_output_dir(), root.join("state/tool-output"));
    assert_eq!(paths.mcp_cache_dir(), root.join("cache/mcp"));
    assert_eq!(paths.socket_path(), root.join("run/daemon.sock"));
    assert_eq!(paths.pid_file(), root.join("run/daemon.pid"));
    assert_eq!(paths.memory_file(), root.join("memory.md"));
    assert_eq!(paths.skills_dir(), root.join("skills"));
}

#[test]
fn ensure_dirs_creates_dirs_and_permissions() {
    let dir = tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());

    paths.ensure_dirs().expect("ensure_dirs must succeed");

    assert!(paths.data_dir.is_dir());
    assert!(paths.state_dir.is_dir());
    assert!(paths.cache_dir.is_dir());
    assert!(paths.runtime_dir.is_dir());
    assert!(paths.spool_dir().is_dir());
    assert!(paths.tool_output_dir().is_dir());
    assert!(paths.mcp_cache_dir().is_dir());

    let meta = fs::metadata(&paths.runtime_dir).expect("metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn logger_writes_line_and_respects_level() {
    let dir = tempdir().expect("tempdir");
    let log_file_path = dir.path().join("logs").join("test.log");

    fs::create_dir_all(log_file_path.parent().expect("parent")).expect("create parent");
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .expect("open file");

    let logger = FileLogger::new(file, log::LevelFilter::Info);

    use log::Log;

    // 1. A Debug record must NOT be written because logger level is Info
    let debug_record = log::Record::builder()
        .args(format_args!("debug message that should be dropped"))
        .level(log::Level::Debug)
        .target("pacode_test::debug")
        .build();
    assert!(!logger.enabled(debug_record.metadata()));
    logger.log(&debug_record);
    logger.flush();

    let initial_contents = fs::read_to_string(&log_file_path).expect("read log file");
    assert!(
        initial_contents.is_empty(),
        "debug record must not be written when filter is Info"
    );

    // 2. A Trace record must NOT be written
    let trace_record = log::Record::builder()
        .args(format_args!("trace message that should be dropped"))
        .level(log::Level::Trace)
        .target("pacode_test::trace")
        .build();
    assert!(!logger.enabled(trace_record.metadata()));
    logger.log(&trace_record);
    logger.flush();

    let trace_contents = fs::read_to_string(&log_file_path).expect("read log file");
    assert!(
        trace_contents.is_empty(),
        "trace record must not be written when filter is Info"
    );

    // 3. An Info record MUST be written
    let info_record = log::Record::builder()
        .args(format_args!("info message 12345"))
        .level(log::Level::Info)
        .target("pacode_test::info")
        .build();
    assert!(logger.enabled(info_record.metadata()));
    logger.log(&info_record);
    logger.flush();

    let info_contents = fs::read_to_string(&log_file_path).expect("read log file");
    assert!(
        info_contents.contains("info message 12345"),
        "info record must be written"
    );
    assert!(info_contents.contains("INFO  pacode_test::info: info message 12345"));

    // 4. A Warn record MUST also be written
    let warn_record = log::Record::builder()
        .args(format_args!("warn message 67890"))
        .level(log::Level::Warn)
        .target("test_warn_target")
        .build();
    assert!(logger.enabled(warn_record.metadata()));
    logger.log(&warn_record);
    logger.flush();

    let warn_contents = fs::read_to_string(&log_file_path).expect("read log file");
    assert!(
        warn_contents.contains("warn message 67890"),
        "warn record must be written"
    );
    assert!(warn_contents.contains("WARN  test_warn_target: warn message 67890"));
}

#[test]
fn level_from_env_all_accepted_and_junk() {
    // Accepted values (case-insensitive and trimmed)
    assert_eq!(level_from_env(Some("error")), Some(log::LevelFilter::Error));
    assert_eq!(level_from_env(Some("ERROR")), Some(log::LevelFilter::Error));
    assert_eq!(level_from_env(Some("Error")), Some(log::LevelFilter::Error));
    assert_eq!(
        level_from_env(Some("  error  ")),
        Some(log::LevelFilter::Error)
    );

    assert_eq!(level_from_env(Some("warn")), Some(log::LevelFilter::Warn));
    assert_eq!(level_from_env(Some("WARN")), Some(log::LevelFilter::Warn));
    assert_eq!(level_from_env(Some("Warn")), Some(log::LevelFilter::Warn));
    assert_eq!(
        level_from_env(Some("  warn  ")),
        Some(log::LevelFilter::Warn)
    );

    assert_eq!(level_from_env(Some("info")), Some(log::LevelFilter::Info));
    assert_eq!(level_from_env(Some("INFO")), Some(log::LevelFilter::Info));
    assert_eq!(level_from_env(Some("Info")), Some(log::LevelFilter::Info));
    assert_eq!(
        level_from_env(Some("  info  ")),
        Some(log::LevelFilter::Info)
    );

    assert_eq!(level_from_env(Some("debug")), Some(log::LevelFilter::Debug));
    assert_eq!(level_from_env(Some("DEBUG")), Some(log::LevelFilter::Debug));
    assert_eq!(level_from_env(Some("Debug")), Some(log::LevelFilter::Debug));
    assert_eq!(
        level_from_env(Some("  debug  ")),
        Some(log::LevelFilter::Debug)
    );

    assert_eq!(level_from_env(Some("trace")), Some(log::LevelFilter::Trace));
    assert_eq!(level_from_env(Some("TRACE")), Some(log::LevelFilter::Trace));
    assert_eq!(level_from_env(Some("Trace")), Some(log::LevelFilter::Trace));
    assert_eq!(
        level_from_env(Some("  trace  ")),
        Some(log::LevelFilter::Trace)
    );

    assert_eq!(level_from_env(Some("off")), Some(log::LevelFilter::Off));
    assert_eq!(level_from_env(Some("OFF")), Some(log::LevelFilter::Off));
    assert_eq!(level_from_env(Some("Off")), Some(log::LevelFilter::Off));
    assert_eq!(level_from_env(Some("  off  ")), Some(log::LevelFilter::Off));

    // Junk values
    assert_eq!(level_from_env(None), None);
    assert_eq!(level_from_env(Some("")), None);
    assert_eq!(level_from_env(Some("   ")), None);
    assert_eq!(level_from_env(Some("junk")), None);
    assert_eq!(level_from_env(Some("invalid")), None);
    assert_eq!(level_from_env(Some("123")), None);
    assert_eq!(level_from_env(Some("warn!")), None);
    assert_eq!(level_from_env(Some("not_a_level")), None);
    assert_eq!(level_from_env(Some("info\0")), None);
}

#[test]
fn default_level_both_profiles() {
    assert_eq!(
        default_level_for(true),
        log::LevelFilter::Trace,
        "debug profile default must be Trace"
    );
    assert_eq!(
        default_level_for(false),
        log::LevelFilter::Warn,
        "release profile default must be Warn"
    );

    let expected = if cfg!(debug_assertions) {
        log::LevelFilter::Trace
    } else {
        log::LevelFilter::Warn
    };
    assert_eq!(default_level(), expected);
}

#[test]
fn compile_time_cap_matches_current_profile() {
    if cfg!(debug_assertions) {
        assert_eq!(
            log::STATIC_MAX_LEVEL,
            log::LevelFilter::Trace,
            "in debug profile, compile-time cap is Trace"
        );
    } else {
        assert_eq!(
            log::STATIC_MAX_LEVEL,
            log::LevelFilter::Warn,
            "in release profile with release_max_level_warn, compile-time cap is Warn"
        );
    }
}

#[test]
#[cfg(not(debug_assertions))]
fn compile_time_cap_in_force_in_release() {
    // Under release builds with feature `release_max_level_warn`:
    // 1. STATIC_MAX_LEVEL is Warn.
    assert_eq!(log::STATIC_MAX_LEVEL, log::LevelFilter::Warn);

    // 2. Macro arguments at Info, Debug, and Trace levels are eliminated at compile time
    // and their expressions are never evaluated.
    let mut side_effect_count = 0;
    let mut side_effect = || {
        side_effect_count += 1;
        "evaluated"
    };

    log::info!("side effect test: {}", side_effect());
    log::debug!("side effect test: {}", side_effect());
    log::trace!("side effect test: {}", side_effect());

    assert_eq!(
        side_effect_count, 0,
        "side effects inside info/debug/trace must not be evaluated in release builds"
    );
}

#[test]
fn resolve_api_key_precedence() {
    // 1. cfg.api_key trimmed non-empty wins
    let mut cfg = ProviderConfig {
        api_key: Some("  secret-direct  ".to_string()),
        api_key_env: Some("TEST_API_KEY_ENV".to_string()),
        ..Default::default()
    };
    // Even if env var is set, inline key takes precedence
    temp_env::with_var("TEST_API_KEY_ENV", Some("from-env"), || {
        assert_eq!(resolve_api_key(&cfg).as_deref(), Some("secret-direct"));
    });

    // 2. cfg.api_key empty falls back to env var
    cfg.api_key = Some("   ".to_string());
    temp_env::with_var("TEST_API_KEY_ENV", Some("  from-env  "), || {
        assert_eq!(resolve_api_key(&cfg).as_deref(), Some("from-env"));
    });

    // 3. Both missing/empty -> None
    cfg.api_key = None;
    temp_env::with_var("TEST_API_KEY_ENV", None::<&str>, || {
        assert_eq!(resolve_api_key(&cfg), None);
    });
}

mod temp_env {
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    pub fn with_var<F, R>(key: &str, val: Option<&str>, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = LOCK.lock().unwrap();
        let prev = std::env::var(key).ok();
        match val {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        let result = f();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        result
    }
}

#[test]
fn load_missing_file_returns_defaults() {
    let dir = tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());
    let cfg = load(&paths).expect("missing config file should load defaults");
    assert_eq!(cfg.provider.effort, Effort::Medium);
}

#[test]
fn load_valid_file_and_parse_error() {
    let dir = tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());

    // Valid file
    fs::write(&paths.config_file, SPEC_EXAMPLE_TOML).expect("write valid config");
    let cfg = load(&paths).expect("valid config must load");
    assert_eq!(
        cfg.provider.default.as_deref(),
        Some("bubna/gemini-3.8-flash")
    );

    // Invalid TOML file
    fs::write(&paths.config_file, "not a valid [[[toml").expect("write invalid config");
    let err = load(&paths).expect_err("invalid toml must return parse error");
    match err {
        ConfigError::Parse { path, message } => {
            assert_eq!(path, paths.config_file);
            assert!(!message.is_empty());
        }
        other => panic!("expected ConfigError::Parse, got: {other:?}"),
    }
}

#[test]
fn prefs_round_trip() {
    let dir = tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());

    // Initially default (missing file)
    let prefs = crate::load_prefs(&paths);
    assert_eq!(prefs, crate::Prefs::default());

    // Save prefs
    let custom = crate::Prefs {
        model: Some("anthropic/claude-3-7-sonnet".to_string()),
        effort: Some(Effort::Max),
        mode: Some(Mode::Bypass),
    };
    crate::save_prefs(&paths, &custom).expect("save_prefs must succeed");

    // Load back and verify
    let loaded = crate::load_prefs(&paths);
    assert_eq!(loaded, custom);

    // Verify written file contains expected keys
    let text = fs::read_to_string(crate::prefs::prefs_file(&paths)).expect("read prefs.toml");
    assert!(text.contains("model = \"anthropic/claude-3-7-sonnet\""));
    assert!(text.contains("effort = \"max\""));
    assert!(text.contains("mode = \"bypass\""));
}

#[test]
fn update_config_value_nested_write() {
    let dir = tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());

    // 1. Write nested value to non-existent file
    crate::update_config_value(&paths, "ui.mouse", toml::Value::Boolean(false))
        .expect("update_config_value should succeed on missing file");

    let cfg = load(&paths).expect("load updated config");
    assert!(!cfg.ui.mouse);

    // 2. Set deeper nested key
    crate::update_config_value(&paths, "ui.hints.effort", toml::Value::Boolean(false))
        .expect("update nested");
    let cfg = load(&paths).expect("load updated config");
    assert!(!cfg.ui.hints.effort);
    assert!(!cfg.ui.mouse);

    // 3. Set top-level / existing table key
    crate::update_config_value(&paths, "exec.yield_after_secs", toml::Value::Integer(42))
        .expect("update integer");
    let cfg = load(&paths).expect("load updated config");
    assert_eq!(cfg.exec.yield_after_secs, 42);
}

#[test]
fn update_and_remove_config_value_keys_round_trip() {
    let dir = tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());

    // 1. Update keys.follow_agent
    crate::update_config_value(
        &paths,
        "keys.follow_agent",
        toml::Value::String("ctrl+a".to_string()),
    )
    .expect("update keys.follow_agent");

    let cfg = load(&paths).expect("load updated config");
    assert_eq!(
        cfg.keys.bindings.get("follow_agent").map(|s| s.as_str()),
        Some("ctrl+a")
    );

    // 2. Remove keys.follow_agent
    crate::remove_config_value(&paths, "keys.follow_agent").expect("remove keys.follow_agent");

    let cfg2 = load(&paths).expect("load updated config");
    assert_eq!(cfg2.keys.bindings.get("follow_agent"), None);
}

#[test]
fn test_ups_toml_parsing() {
    // 1. Integer form
    let cfg1 = parse("[ui]\nups = 10\n").expect("parse ups = 10");
    assert_eq!(cfg1.ui.ups, pacode_types::Ups::Fixed(10));

    let cfg2 = parse("[ui]\nups = 60\n").expect("parse ups = 60");
    assert_eq!(cfg2.ui.ups, pacode_types::Ups::Fixed(60));

    // 2. String form: dynamic
    let cfg3 = parse("[ui]\nups = \"dynamic\"\n").expect("parse ups = dynamic");
    assert_eq!(cfg3.ui.ups, pacode_types::Ups::Dynamic);

    // 3. String form: auto
    let cfg4 = parse("[ui]\nups = \"auto\"\n").expect("parse ups = auto");
    assert_eq!(cfg4.ui.ups, pacode_types::Ups::Auto);

    // 4. Default when omitted
    let cfg5 = parse("[ui]\nmouse = false\n").expect("parse without ups");
    assert_eq!(cfg5.ui.ups, pacode_types::Ups::Fixed(10));

    // 5. Invalid values must fail
    assert!(parse("[ui]\nups = \"invalid\"\n").is_err());
    assert!(parse("[ui]\nups = -1\n").is_err());
}

#[test]
fn test_ups_frame_interval_clamping() {
    use std::time::Duration;

    // Fixed values and clamping
    assert_eq!(
        pacode_types::Ups::Fixed(0).frame_interval(None),
        Some(Duration::from_millis(1000))
    );
    assert_eq!(
        pacode_types::Ups::Fixed(10).frame_interval(None),
        Some(Duration::from_millis(100))
    );
    assert_eq!(
        pacode_types::Ups::Fixed(60).frame_interval(None),
        Some(Duration::from_millis(16))
    );
    assert_eq!(
        pacode_types::Ups::Fixed(300).frame_interval(None),
        Some(Duration::from_millis(4))
    );

    // Dynamic always returns None
    assert_eq!(pacode_types::Ups::Dynamic.frame_interval(None), None);
    assert_eq!(pacode_types::Ups::Dynamic.frame_interval(Some(60)), None);

    // Auto with detected Hz vs fallback
    assert_eq!(
        pacode_types::Ups::Auto.frame_interval(None),
        Some(Duration::from_millis(100))
    );
    assert_eq!(
        pacode_types::Ups::Auto.frame_interval(Some(60)),
        Some(Duration::from_millis(16))
    );
    assert_eq!(
        pacode_types::Ups::Auto.frame_interval(Some(144)),
        Some(Duration::from_millis(6))
    );
    assert_eq!(
        pacode_types::Ups::Auto.frame_interval(Some(0)),
        Some(Duration::from_millis(1000))
    );
    assert_eq!(
        pacode_types::Ups::Auto.frame_interval(Some(360)),
        Some(Duration::from_millis(4))
    );
}

#[test]
fn dependency_records_are_held_to_warn_even_at_trace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.log");
    let file = std::fs::File::create(&path).expect("create");
    let logger = FileLogger::new(file, log::LevelFilter::Trace);

    // Our own crates get the configured level...
    let ours = log::Metadata::builder()
        .level(log::Level::Trace)
        .target("pacode_tui::keys")
        .build();
    assert!(log::Log::enabled(&logger, &ours));

    // ...while a dependency's trace and debug are dropped, so an event loop
    // cannot bury the log in mio/tokio chatter.
    for target in [
        "mio::poll",
        "tokio::runtime",
        "rustls::client",
        "hyper::proto",
    ] {
        for level in [log::Level::Trace, log::Level::Debug, log::Level::Info] {
            let dep = log::Metadata::builder().level(level).target(target).build();
            assert!(
                !log::Log::enabled(&logger, &dep),
                "{target} at {level} should be filtered"
            );
        }
        // Their warnings and errors still matter.
        let warn = log::Metadata::builder()
            .level(log::Level::Warn)
            .target(target)
            .build();
        assert!(log::Log::enabled(&logger, &warn), "{target} warn must pass");
    }
}

#[test]
fn a_quiet_setting_also_silences_dependencies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.log");
    let file = std::fs::File::create(&path).expect("create");
    let logger = FileLogger::new(file, log::LevelFilter::Error);

    let dep = log::Metadata::builder()
        .level(log::Level::Warn)
        .target("mio::poll")
        .build();
    assert!(
        !log::Log::enabled(&logger, &dep),
        "PACODE_LOG=error must not be raised back to warn for dependencies"
    );
}
