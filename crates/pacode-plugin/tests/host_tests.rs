use std::path::PathBuf;
use std::sync::Arc;

use pacode_plugin::sink::NoopUiSink;
use pacode_plugin::types::{
    CommandOutcome, HookEvent, HookResult, PluginCommandDef, PluginKind, PluginToolDef,
};
use pacode_plugin::{PluginError, PluginHost, PluginRuntime};
use pacode_types::config::PluginsConfig;
use serde_json::json;

struct DummyPlugin {
    name: String,
    tools: Vec<PluginToolDef>,
    commands: Vec<PluginCommandDef>,
    hook_behavior: HookBehavior,
}

enum HookBehavior {
    Pass,
    Deny(String),
    Modify(serde_json::Value),
}

#[async_trait::async_trait]
impl PluginRuntime for DummyPlugin {
    fn name(&self) -> &str {
        &self.name
    }
    fn version(&self) -> &str {
        "1.0.0"
    }
    fn kind(&self) -> PluginKind {
        PluginKind::Lua
    }
    fn description(&self) -> Option<&str> {
        Some("dummy")
    }
    fn tools(&self) -> Vec<PluginToolDef> {
        self.tools.clone()
    }
    fn commands(&self) -> Vec<PluginCommandDef> {
        self.commands.clone()
    }
    async fn call_tool(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        if self.tools.iter().any(|t| t.name == name) {
            Ok(json!({ "result": input }))
        } else {
            Err(PluginError::ToolNotFound {
                name: name.to_string(),
            })
        }
    }
    async fn run_command(&self, name: &str, args: String) -> Result<CommandOutcome, PluginError> {
        if self.commands.iter().any(|c| c.name == name) {
            Ok(CommandOutcome::InsertText(format!("run {name}: {args}")))
        } else {
            Err(PluginError::CommandNotFound {
                name: name.to_string(),
            })
        }
    }
    async fn hook(&self, event: HookEvent) -> Result<HookResult, PluginError> {
        match &self.hook_behavior {
            HookBehavior::Pass => Ok(HookResult::Continue),
            HookBehavior::Deny(reason) => Ok(HookResult::Deny {
                reason: reason.clone(),
            }),
            HookBehavior::Modify(new_val) => {
                let mut base = match event {
                    HookEvent::PreToolCall { input, .. } => input,
                    _ => json!({}),
                };
                if let (Some(b_obj), Some(n_obj)) = (base.as_object_mut(), new_val.as_object()) {
                    for (k, v) in n_obj {
                        b_obj.insert(k.clone(), v.clone());
                    }
                }
                Ok(HookResult::ModifyInput(base))
            }
        }
    }
}

#[tokio::test]
async fn test_host_load_from_dir() {
    let fix_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    let config = PluginsConfig {
        enabled: true,
        dirs: vec![fix_dir],
        wasm_memory_mb: 64,
        lua_memory_mb: 32,
        hook_timeout_ms: 2000,
    };

    let host = PluginHost::load(&config, Arc::new(NoopUiSink)).await;
    assert_eq!(host.plugins.len(), 1);
    assert_eq!(host.plugins[0].name(), "echo-lua");

    let tools = host.tools();
    assert_eq!(tools.len(), 3);
    let commands = host.commands();
    assert_eq!(commands.len(), 3);

    let list = host.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "echo-lua");
    assert_eq!(list[0].kind, Some(PluginKind::Lua));
    assert!(list[0].error.is_none());

    // Call tool through host
    let res = host
        .call_tool("echo_tool", json!({ "k": "v" }))
        .await
        .unwrap();
    assert_eq!(res["k"], "v");

    // Call command through host
    let cmd_res = host
        .run_command("echo_cmd", "host_arg".to_string())
        .await
        .unwrap();
    assert_eq!(
        cmd_res,
        CommandOutcome::InsertText("inserted: host_arg".to_string())
    );
}

#[tokio::test]
async fn test_host_disabled_config() {
    let fix_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    let config = PluginsConfig {
        enabled: false,
        dirs: vec![fix_dir],
        wasm_memory_mb: 64,
        lua_memory_mb: 32,
        hook_timeout_ms: 2000,
    };

    let host = PluginHost::load(&config, Arc::new(NoopUiSink)).await;
    assert!(host.plugins.is_empty());
    assert!(host.tools().is_empty());
}

#[tokio::test]
async fn test_host_run_hooks_first_deny_wins() {
    let mut host = PluginHost::new();

    host.plugins.push(Box::new(DummyPlugin {
        name: "p1".to_string(),
        tools: vec![],
        commands: vec![],
        hook_behavior: HookBehavior::Pass,
    }));
    host.plugins.push(Box::new(DummyPlugin {
        name: "p2".to_string(),
        tools: vec![],
        commands: vec![],
        hook_behavior: HookBehavior::Deny("denied by p2".to_string()),
    }));
    host.plugins.push(Box::new(DummyPlugin {
        name: "p3".to_string(),
        tools: vec![],
        commands: vec![],
        hook_behavior: HookBehavior::Deny("denied by p3".to_string()),
    }));

    let event = HookEvent::PreToolCall {
        name: "sample_tool".to_string(),
        input: json!({ "x": 1 }),
    };
    let result = host.run_hooks(&event).await.unwrap();

    assert_eq!(
        result,
        HookResult::Deny {
            reason: "denied by p2".to_string()
        }
    );
}

#[tokio::test]
async fn test_host_run_hooks_modify_input_chains() {
    let mut host = PluginHost::new();

    host.plugins.push(Box::new(DummyPlugin {
        name: "p1".to_string(),
        tools: vec![],
        commands: vec![],
        hook_behavior: HookBehavior::Modify(json!({ "p1_added": true })),
    }));
    host.plugins.push(Box::new(DummyPlugin {
        name: "p2".to_string(),
        tools: vec![],
        commands: vec![],
        hook_behavior: HookBehavior::Modify(json!({ "p2_added": "yes" })),
    }));
    host.plugins.push(Box::new(DummyPlugin {
        name: "p3".to_string(),
        tools: vec![],
        commands: vec![],
        hook_behavior: HookBehavior::Pass,
    }));

    let event = HookEvent::PreToolCall {
        name: "sample_tool".to_string(),
        input: json!({ "initial": 42 }),
    };
    let result = host.run_hooks(&event).await.unwrap();

    match result {
        HookResult::ModifyInput(val) => {
            assert_eq!(val["initial"], 42);
            assert_eq!(val["p1_added"], true);
            assert_eq!(val["p2_added"], "yes");
        }
        _ => panic!("expected ModifyInput with chained results"),
    }
}

#[tokio::test]
async fn test_host_tool_not_found() {
    let host = PluginHost::new();
    let res = host.call_tool("nonexistent", json!({})).await;
    assert!(matches!(res, Err(PluginError::ToolNotFound { .. })));
}

#[tokio::test]
async fn test_host_command_not_found() {
    let host = PluginHost::new();
    let res = host.run_command("nonexistent", "args".to_string()).await;
    assert!(matches!(res, Err(PluginError::CommandNotFound { .. })));
}

#[tokio::test]
async fn test_host_empty_run_hooks() {
    let host = PluginHost::new();
    let event = HookEvent::TurnStart;
    let res = host.run_hooks(&event).await.unwrap();
    assert_eq!(res, HookResult::Continue);
}

#[tokio::test]
async fn test_host_invalid_manifest_records_error() {
    let temp = tempfile::tempdir().unwrap();
    let bad_plugin_dir = temp.path().join("bad-plugin");
    std::fs::create_dir_all(&bad_plugin_dir).unwrap();
    std::fs::write(bad_plugin_dir.join("plugin.toml"), "invalid toml :::").unwrap();

    let config = PluginsConfig {
        enabled: true,
        dirs: vec![temp.path().to_path_buf()],
        wasm_memory_mb: 64,
        lua_memory_mb: 32,
        hook_timeout_ms: 2000,
    };

    let host = PluginHost::load(&config, Arc::new(NoopUiSink)).await;
    assert!(host.plugins.is_empty());
    assert_eq!(host.errors.len(), 1);
    let list = host.list();
    assert_eq!(list.len(), 1);
    assert!(list[0].error.is_some());
}
