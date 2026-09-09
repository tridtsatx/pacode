use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pacode_plugin::sink::UiSink;
use pacode_plugin::types::{CommandOutcome, HookEvent, HookResult, PluginManifest};
use pacode_plugin::{LuaPlugin, PluginError, PluginRuntime};
use serde_json::json;

#[derive(Default, Clone)]
struct TestUiSink {
    toasts: Arc<std::sync::Mutex<Vec<String>>>,
    statuses: Arc<std::sync::Mutex<Vec<String>>>,
}

impl UiSink for TestUiSink {
    fn toast(&self, text: &str) {
        self.toasts.lock().unwrap().push(text.to_string());
    }

    fn status(&self, text: &str) {
        self.statuses.lock().unwrap().push(text.to_string());
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("echo-lua")
}

#[tokio::test]
async fn test_lua_plugin_load_and_definitions() {
    let sink = Arc::new(TestUiSink::default());
    let fix = fixture_path();
    let manifest_str = std::fs::read_to_string(fix.join("plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();

    let plugin = LuaPlugin::load(&manifest, &fix.join("main.lua"), 32, 2000, sink).unwrap();

    assert_eq!(plugin.name(), "echo-lua");
    assert_eq!(plugin.version(), "0.1.0");

    let tools = plugin.tools();
    assert_eq!(tools.len(), 3);
    assert!(tools.iter().any(|t| t.name == "echo_tool"));
    assert!(tools.iter().any(|t| t.name == "allocate_mem"));
    assert!(tools.iter().any(|t| t.name == "infinite_loop"));

    let commands = plugin.commands();
    assert_eq!(commands.len(), 3);
    assert!(commands.iter().any(|c| c.name == "echo_cmd"));
    assert!(commands.iter().any(|c| c.name == "prompt_cmd"));
    assert!(commands.iter().any(|c| c.name == "toast_cmd"));
}

#[tokio::test]
async fn test_lua_plugin_tool_call() {
    let sink = Arc::new(TestUiSink::default());
    let fix = fixture_path();
    let manifest_str = std::fs::read_to_string(fix.join("plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();

    let plugin = LuaPlugin::load(&manifest, &fix.join("main.lua"), 32, 2000, sink.clone()).unwrap();

    let input = json!({ "msg": "hello world", "trigger_toast": true });
    let res = plugin.call_tool("echo_tool", input.clone()).await.unwrap();
    assert_eq!(res["msg"], "hello world");

    let toasts = sink.toasts.lock().unwrap().clone();
    assert_eq!(toasts, vec!["hello from echo_tool"]);
    let statuses = sink.statuses.lock().unwrap().clone();
    assert_eq!(statuses, vec!["running echo_tool"]);
}

#[tokio::test]
async fn test_lua_plugin_commands() {
    let sink = Arc::new(TestUiSink::default());
    let fix = fixture_path();
    let manifest_str = std::fs::read_to_string(fix.join("plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();

    let plugin = LuaPlugin::load(&manifest, &fix.join("main.lua"), 32, 2000, sink.clone()).unwrap();

    let out1 = plugin
        .run_command("echo_cmd", "foo".to_string())
        .await
        .unwrap();
    assert_eq!(
        out1,
        CommandOutcome::InsertText("inserted: foo".to_string())
    );

    let out2 = plugin
        .run_command("prompt_cmd", "bar".to_string())
        .await
        .unwrap();
    assert_eq!(out2, CommandOutcome::SendPrompt("sent: bar".to_string()));

    let out3 = plugin
        .run_command("toast_cmd", "baz".to_string())
        .await
        .unwrap();
    assert_eq!(out3, CommandOutcome::Nothing);

    let toasts = sink.toasts.lock().unwrap().clone();
    assert_eq!(toasts, vec!["toast: baz"]);
}

#[tokio::test]
async fn test_lua_plugin_pre_tool_call_deny_and_modify() {
    let sink = Arc::new(TestUiSink::default());
    let fix = fixture_path();
    let manifest_str = std::fs::read_to_string(fix.join("plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();

    let plugin = LuaPlugin::load(&manifest, &fix.join("main.lua"), 32, 2000, sink.clone()).unwrap();

    // 1. Normal continue
    let normal_event = HookEvent::PreToolCall {
        name: "test_tool".to_string(),
        input: json!({ "msg": "ok" }),
    };
    let res1 = plugin.hook(normal_event).await.unwrap();
    assert_eq!(res1, HookResult::Continue);

    // 2. Deny
    let deny_event = HookEvent::PreToolCall {
        name: "test_tool".to_string(),
        input: json!({ "deny": true }),
    };
    let res2 = plugin.hook(deny_event).await.unwrap();
    assert_eq!(
        res2,
        HookResult::Deny {
            reason: "denied by pre_tool_call hook".to_string()
        }
    );

    // 3. Modify
    let modify_event = HookEvent::PreToolCall {
        name: "test_tool".to_string(),
        input: json!({ "modify": true, "key": "val" }),
    };
    let res3 = plugin.hook(modify_event).await.unwrap();
    match res3 {
        HookResult::ModifyInput(new_input) => {
            assert_eq!(new_input["modified"], true);
            assert_eq!(new_input["injected"], "extra_field");
        }
        _ => panic!("expected ModifyInput"),
    }

    // 4. Toast via hook
    let toast_event = HookEvent::PreToolCall {
        name: "test_tool".to_string(),
        input: json!({ "toast_pre": true }),
    };
    let _ = plugin.hook(toast_event).await.unwrap();
    let toasts = sink.toasts.lock().unwrap().clone();
    assert_eq!(toasts, vec!["toast from pre_tool_call"]);
}

#[tokio::test]
async fn test_lua_plugin_memory_limit_error() {
    let sink = Arc::new(TestUiSink::default());
    let fix = fixture_path();
    let manifest_str = std::fs::read_to_string(fix.join("plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();

    // Set memory limit to a small 2 MB
    let plugin = LuaPlugin::load(&manifest, &fix.join("main.lua"), 2, 2000, sink).unwrap();

    let res = plugin.call_tool("allocate_mem", json!({})).await;
    assert!(res.is_err());
    match res.unwrap_err() {
        PluginError::MemoryLimit(msg) => {
            assert!(msg.contains("memory"), "expected memory in message: {msg}");
        }
        err => panic!("expected MemoryLimit error, got {err:?}"),
    }
}

#[tokio::test]
async fn test_lua_plugin_timeout_via_infinite_loop() {
    let sink = Arc::new(TestUiSink::default());
    let fix = fixture_path();
    let manifest_str = std::fs::read_to_string(fix.join("plugin.toml")).unwrap();
    let manifest: PluginManifest = toml::from_str(&manifest_str).unwrap();

    // Short timeout of 100ms
    let plugin = LuaPlugin::load(&manifest, &fix.join("main.lua"), 32, 100, sink).unwrap();

    let start = std::time::Instant::now();
    let res = plugin.call_tool("infinite_loop", json!({})).await;
    let elapsed = start.elapsed();

    assert!(res.is_err());
    match res.unwrap_err() {
        PluginError::Timeout => {}
        err => panic!("expected Timeout error, got {err:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout should trigger quickly, elapsed: {elapsed:?}"
    );
}
