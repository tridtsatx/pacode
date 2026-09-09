//! Lua plugin implementation using `mlua`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mlua::{HookTriggers, Lua, LuaSerdeExt, Value as LuaValue, VmState};
use serde_json::Value;

use crate::error::PluginError;
use crate::runtime::PluginRuntime;
use crate::sink::UiSink;
use crate::types::{
    CommandOutcome, HookEvent, HookResult, PluginCommandDef, PluginKind, PluginManifest,
    PluginToolDef,
};

pub struct LuaPlugin {
    manifest: PluginManifest,
    tools: Vec<PluginToolDef>,
    commands: Vec<PluginCommandDef>,
    lua: Arc<std::sync::Mutex<Lua>>,
    timeout: Duration,
}

impl LuaPlugin {
    pub fn load(
        manifest: &PluginManifest,
        entry_path: &Path,
        lua_memory_mb: u32,
        hook_timeout_ms: u64,
        ui_sink: Arc<dyn UiSink>,
    ) -> Result<Self, PluginError> {
        let script = std::fs::read_to_string(entry_path).map_err(PluginError::Io)?;

        let lua = Lua::new();
        let mem_limit_bytes = (lua_memory_mb as usize).saturating_mul(1024 * 1024);
        let _ = lua.set_memory_limit(mem_limit_bytes);

        // Inject pacode globals
        let pacode = lua.create_table().map_err(map_lua_error)?;
        let sink_toast = ui_sink.clone();
        pacode
            .set(
                "toast",
                lua.create_function(move |_, text: String| {
                    sink_toast.toast(&text);
                    Ok(())
                })
                .map_err(map_lua_error)?,
            )
            .map_err(map_lua_error)?;

        let sink_status = ui_sink.clone();
        pacode
            .set(
                "status",
                lua.create_function(move |_, text: String| {
                    sink_status.status(&text);
                    Ok(())
                })
                .map_err(map_lua_error)?,
            )
            .map_err(map_lua_error)?;

        lua.globals().set("pacode", pacode).map_err(map_lua_error)?;

        // Execute plugin script
        let plugin_table: mlua::Table = lua
            .load(&script)
            .set_name(entry_path.to_string_lossy())
            .eval()
            .map_err(map_lua_error)?;

        // Extract tools definitions
        let mut tools = Vec::new();
        if let Ok(tools_table) = plugin_table.get::<mlua::Table>("tools") {
            for tool_tab in tools_table.sequence_values::<mlua::Table>().flatten() {
                let name: String = tool_tab.get("name").map_err(map_lua_error)?;
                let description: String = tool_tab.get("description").unwrap_or_default();
                let schema: Value = match tool_tab.get::<LuaValue>("schema") {
                    Ok(v) => lua.from_value(v).unwrap_or(Value::Null),
                    Err(_) => Value::Null,
                };
                tools.push(PluginToolDef {
                    name,
                    description,
                    schema,
                });
            }
        }

        // Extract commands definitions
        let mut commands = Vec::new();
        if let Ok(commands_table) = plugin_table.get::<mlua::Table>("commands") {
            for cmd_tab in commands_table.sequence_values::<mlua::Table>().flatten() {
                let name: String = cmd_tab.get("name").map_err(map_lua_error)?;
                let description: String = cmd_tab.get("description").unwrap_or_default();
                commands.push(PluginCommandDef { name, description });
            }
        }

        // Store root plugin table in registry
        lua.set_named_registry_value("__pacode_plugin", plugin_table)
            .map_err(map_lua_error)?;

        Ok(Self {
            manifest: manifest.clone(),
            tools,
            commands,
            lua: Arc::new(std::sync::Mutex::new(lua)),
            timeout: Duration::from_millis(hook_timeout_ms),
        })
    }

    async fn run_blocking<F, R>(&self, f: F) -> Result<R, PluginError>
    where
        F: FnOnce(&Lua) -> Result<R, PluginError> + Send + 'static,
        R: Send + 'static,
    {
        let lua_arc = self.lua.clone();
        let timeout = self.timeout;

        let task = tokio::task::spawn_blocking(move || {
            let lua = lua_arc
                .lock()
                .map_err(|e| PluginError::Lua(e.to_string()))?;
            let deadline = Instant::now() + timeout;

            // Abortion hook for runaway scripts
            let _ = lua.set_hook(
                HookTriggers::default().every_nth_instruction(1000),
                move |_lua, _debug| {
                    if Instant::now() >= deadline {
                        Err(mlua::Error::runtime("__PACODE_TIMEOUT__"))
                    } else {
                        Ok(VmState::Continue)
                    }
                },
            );

            let result = f(&lua);
            lua.remove_hook();
            result
        });

        match tokio::time::timeout(timeout, task).await {
            Ok(Ok(res)) => res,
            Ok(Err(join_err)) => Err(PluginError::Lua(join_err.to_string())),
            Err(_) => Err(PluginError::Timeout),
        }
    }
}

#[async_trait]
impl PluginRuntime for LuaPlugin {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn version(&self) -> &str {
        &self.manifest.version
    }

    fn kind(&self) -> PluginKind {
        PluginKind::Lua
    }

    fn description(&self) -> Option<&str> {
        self.manifest.description.as_deref()
    }

    fn tools(&self) -> Vec<PluginToolDef> {
        self.tools.clone()
    }

    fn commands(&self) -> Vec<PluginCommandDef> {
        self.commands.clone()
    }

    async fn call_tool(&self, tool_name: &str, input: Value) -> Result<Value, PluginError> {
        let name_owned = tool_name.to_string();
        self.run_blocking(move |lua| {
            let plugin_table: mlua::Table = lua
                .named_registry_value("__pacode_plugin")
                .map_err(map_lua_error)?;
            let tools_table: mlua::Table =
                plugin_table
                    .get("tools")
                    .map_err(|_| PluginError::ToolNotFound {
                        name: name_owned.clone(),
                    })?;

            let mut handler = None;
            for tool_tab in tools_table.sequence_values::<mlua::Table>().flatten() {
                let Ok(name) = tool_tab.get::<String>("name") else {
                    continue;
                };
                if name == name_owned {
                    handler = tool_tab.get::<mlua::Function>("handler").ok();
                    break;
                }
            }

            let Some(func) = handler else {
                return Err(PluginError::ToolNotFound { name: name_owned });
            };

            let lua_input = lua.to_value(&input).map_err(map_lua_error)?;
            let result: LuaValue = func.call(lua_input).map_err(map_lua_error)?;
            lua.from_value(result).map_err(map_lua_error)
        })
        .await
    }

    async fn run_command(
        &self,
        cmd_name: &str,
        args: String,
    ) -> Result<CommandOutcome, PluginError> {
        let name_owned = cmd_name.to_string();
        self.run_blocking(move |lua| {
            let plugin_table: mlua::Table = lua
                .named_registry_value("__pacode_plugin")
                .map_err(map_lua_error)?;
            let commands_table: mlua::Table =
                plugin_table
                    .get("commands")
                    .map_err(|_| PluginError::CommandNotFound {
                        name: name_owned.clone(),
                    })?;

            let mut handler = None;
            for cmd_tab in commands_table.sequence_values::<mlua::Table>().flatten() {
                let Ok(name) = cmd_tab.get::<String>("name") else {
                    continue;
                };
                if name == name_owned {
                    handler = cmd_tab.get::<mlua::Function>("handler").ok();
                    break;
                }
            }

            let Some(func) = handler else {
                return Err(PluginError::CommandNotFound { name: name_owned });
            };

            let result: LuaValue = func.call(args).map_err(map_lua_error)?;
            parse_lua_command_outcome(lua, result)
        })
        .await
    }

    async fn hook(&self, event: HookEvent) -> Result<HookResult, PluginError> {
        self.run_blocking(move |lua| {
            let plugin_table: mlua::Table = lua
                .named_registry_value("__pacode_plugin")
                .map_err(map_lua_error)?;
            let Ok(hooks_table) = plugin_table.get::<mlua::Table>("hooks") else {
                return Ok(HookResult::Continue);
            };

            match event {
                HookEvent::PreToolCall { name, input } => {
                    let Ok(func) = hooks_table.get::<mlua::Function>("pre_tool_call") else {
                        return Ok(HookResult::Continue);
                    };
                    let lua_input = lua.to_value(&input).map_err(map_lua_error)?;
                    let result: LuaValue = func.call((name, lua_input)).map_err(map_lua_error)?;
                    parse_lua_hook_result(lua, result)
                }
                HookEvent::PostToolCall {
                    name,
                    input,
                    output,
                } => {
                    let Ok(func) = hooks_table.get::<mlua::Function>("post_tool_call") else {
                        return Ok(HookResult::Continue);
                    };
                    let lua_input = lua.to_value(&input).map_err(map_lua_error)?;
                    let lua_output = lua.to_value(&output).map_err(map_lua_error)?;
                    let result: LuaValue = func
                        .call((name, lua_input, lua_output))
                        .map_err(map_lua_error)?;
                    parse_lua_hook_result(lua, result)
                }
                HookEvent::TurnStart => {
                    let Ok(func) = hooks_table.get::<mlua::Function>("turn_start") else {
                        return Ok(HookResult::Continue);
                    };
                    let result: LuaValue = func.call(()).map_err(map_lua_error)?;
                    parse_lua_hook_result(lua, result)
                }
                HookEvent::TurnEnd {
                    duration_ms,
                    output_tokens,
                } => {
                    let Ok(func) = hooks_table.get::<mlua::Function>("turn_end") else {
                        return Ok(HookResult::Continue);
                    };
                    let stats = lua.create_table().map_err(map_lua_error)?;
                    stats
                        .set("duration_ms", duration_ms)
                        .map_err(map_lua_error)?;
                    stats
                        .set("output_tokens", output_tokens)
                        .map_err(map_lua_error)?;
                    let result: LuaValue = func.call(stats).map_err(map_lua_error)?;
                    parse_lua_hook_result(lua, result)
                }
                HookEvent::OnMessage { role, text } => {
                    let Ok(func) = hooks_table.get::<mlua::Function>("on_message") else {
                        return Ok(HookResult::Continue);
                    };
                    let result: LuaValue = func.call((role, text)).map_err(map_lua_error)?;
                    parse_lua_hook_result(lua, result)
                }
            }
        })
        .await
    }
}

fn map_lua_error(err: mlua::Error) -> PluginError {
    match &err {
        mlua::Error::MemoryError(_) => PluginError::MemoryLimit(err.to_string()),
        mlua::Error::RuntimeError(msg) if msg.contains("__PACODE_TIMEOUT__") => {
            PluginError::Timeout
        }
        _ => {
            let msg = err.to_string();
            if msg.contains("not enough memory") || msg.contains("memory limit") {
                PluginError::MemoryLimit(msg)
            } else if msg.contains("__PACODE_TIMEOUT__") {
                PluginError::Timeout
            } else {
                PluginError::Lua(msg)
            }
        }
    }
}

fn parse_lua_command_outcome(lua: &Lua, val: LuaValue) -> Result<CommandOutcome, PluginError> {
    match val {
        LuaValue::Nil => Ok(CommandOutcome::Nothing),
        LuaValue::String(s) => {
            let str_val = s.to_str().map_err(map_lua_error)?;
            if str_val.as_ref() == "nothing" {
                Ok(CommandOutcome::Nothing)
            } else {
                Ok(CommandOutcome::InsertText(str_val.to_string()))
            }
        }
        LuaValue::Table(tab) => {
            if let Ok(text) = tab.get::<String>("insert_text") {
                return Ok(CommandOutcome::InsertText(text));
            }
            if let Ok(prompt) = tab.get::<String>("send_prompt") {
                return Ok(CommandOutcome::SendPrompt(prompt));
            }
            if let Ok(typ) = tab.get::<String>("type") {
                let text = tab.get::<String>("text").unwrap_or_default();
                match typ.as_str() {
                    "insert_text" => return Ok(CommandOutcome::InsertText(text)),
                    "send_prompt" => return Ok(CommandOutcome::SendPrompt(text)),
                    "nothing" => return Ok(CommandOutcome::Nothing),
                    _ => {}
                }
            }
            if tab.contains_key("nothing").unwrap_or(false) {
                return Ok(CommandOutcome::Nothing);
            }
            let json_val: Value = lua
                .from_value(LuaValue::Table(tab))
                .map_err(map_lua_error)?;
            serde_json::from_value(json_val).map_err(|e| PluginError::Serialization(e.to_string()))
        }
        _ => Ok(CommandOutcome::Nothing),
    }
}

fn parse_lua_hook_result(lua: &Lua, val: LuaValue) -> Result<HookResult, PluginError> {
    match val {
        LuaValue::Nil => Ok(HookResult::Continue),
        LuaValue::Boolean(false) => Ok(HookResult::Continue),
        LuaValue::Boolean(true) => Ok(HookResult::Continue),
        LuaValue::String(s) => {
            let str_val = s.to_str().map_err(map_lua_error)?;
            if str_val.as_ref() == "continue" {
                Ok(HookResult::Continue)
            } else {
                Ok(HookResult::Deny {
                    reason: str_val.to_string(),
                })
            }
        }
        LuaValue::Table(tab) => {
            if let Ok(reason) = tab.get::<String>("deny") {
                return Ok(HookResult::Deny { reason });
            }
            if let Ok(reason) = tab.get::<String>("error") {
                return Ok(HookResult::Deny { reason });
            }
            if let Ok(modify_input) = tab.get::<LuaValue>("modify_input") {
                let json_val: Value = lua.from_value(modify_input).map_err(map_lua_error)?;
                return Ok(HookResult::ModifyInput(json_val));
            }
            if let Ok(typ) = tab.get::<String>("type") {
                match typ.as_str() {
                    "continue" => return Ok(HookResult::Continue),
                    "deny" => {
                        let reason = tab
                            .get::<String>("reason")
                            .unwrap_or_else(|_| "denied".to_string());
                        return Ok(HookResult::Deny { reason });
                    }
                    "modify_input" | "modify" => {
                        let input = tab
                            .get::<LuaValue>("input")
                            .ok()
                            .and_then(|v| lua.from_value(v).ok())
                            .unwrap_or(Value::Null);
                        return Ok(HookResult::ModifyInput(input));
                    }
                    _ => {}
                }
            }
            if tab.contains_key("continue").unwrap_or(false) {
                return Ok(HookResult::Continue);
            }
            let json_val: Value = lua
                .from_value(LuaValue::Table(tab))
                .map_err(map_lua_error)?;
            serde_json::from_value(json_val).map_err(|e| PluginError::Serialization(e.to_string()))
        }
        _ => Ok(HookResult::Continue),
    }
}
