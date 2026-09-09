use std::fs;
use std::os::unix::fs::PermissionsExt;

use pacode_types::{Config, Effort, Mode, ProviderConfig};
use tempfile::tempdir;

use crate::logging::{FileLogger, default_level, init_file_logger, level_from_env};
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
fn logger_writes_line_containing_message() {
    let dir = tempdir().expect("tempdir");
    let log_file_path = dir.path().join("logs").join("test.log");

    fs::create_dir_all(log_file_path.parent().expect("parent")).expect("create parent");
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .expect("open file");

    let logger = FileLogger::new(file, log::LevelFilter::Info);

    let target_name = "test_target";
    let message_text = "test logging message 12345";
    let args = format_args!("{message_text}");
    let record = log::Record::builder()
        .args(args)
        .level(log::Level::Info)
        .target(target_name)
        .module_path(Some("pacode_config::tests"))
        .file(Some("config_tests.rs"))
        .line(Some(42))
        .build();

    use log::Log;
    logger.log(&record);
    logger.flush();

    let contents = fs::read_to_string(&log_file_path).expect("read log file");
    assert!(
        contents.contains(message_text),
        "log file should contain message: {contents}"
    );
    assert!(
        contents.contains("INFO"),
        "log file should contain level: {contents}"
    );
    assert!(
        contents.contains(target_name),
        "log file should contain target: {contents}"
    );
    // Line format: 2026-09-09T12:34:56.789Z INFO  target: message
    assert!(
        contents.contains("INFO  test_target: test logging message 12345"),
        "log line must follow the exact format: {contents}"
    );
}

#[test]
fn init_file_logger_integration_and_second_call() {
    let dir = tempdir().expect("tempdir");
    let log_path = dir.path().join("nested").join("daemon.log");

    init_file_logger(&log_path, log::LevelFilter::Debug).expect("first init");
    init_file_logger(&log_path, log::LevelFilter::Debug).expect("second init should succeed");

    log::info!("integration test info line");

    // Give IO a moment if needed
    let _ = fs::read_to_string(&log_path);
}

#[test]
fn level_from_env_parsing() {
    assert_eq!(level_from_env(None), None);
    assert_eq!(level_from_env(Some("")), None);
    assert_eq!(level_from_env(Some("invalid")), None);

    assert_eq!(level_from_env(Some("error")), Some(log::LevelFilter::Error));
    assert_eq!(level_from_env(Some("ERROR")), Some(log::LevelFilter::Error));
    assert_eq!(level_from_env(Some("warn")), Some(log::LevelFilter::Warn));
    assert_eq!(level_from_env(Some("Warn")), Some(log::LevelFilter::Warn));
    assert_eq!(level_from_env(Some("info")), Some(log::LevelFilter::Info));
    assert_eq!(level_from_env(Some(" INFO ")), Some(log::LevelFilter::Info));
    assert_eq!(level_from_env(Some("debug")), Some(log::LevelFilter::Debug));
    assert_eq!(level_from_env(Some("trace")), Some(log::LevelFilter::Trace));
    assert_eq!(level_from_env(Some("off")), Some(log::LevelFilter::Off));
}

#[test]
fn default_level_matches_build_profile() {
    let expected = if cfg!(debug_assertions) {
        log::LevelFilter::Trace
    } else {
        log::LevelFilter::Warn
    };
    assert_eq!(default_level(), expected);
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
