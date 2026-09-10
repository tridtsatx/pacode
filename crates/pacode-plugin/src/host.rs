//! Plugin host managing discovery, loading, tools, commands, and hooks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pacode_types::config::PluginsConfig;
use serde_json::Value;

use crate::error::PluginError;
use crate::lua::LuaPlugin;
use crate::runtime::PluginRuntime;
use crate::sink::UiSink;
use crate::types::{
    CommandOutcome, HookEvent, HookResult, PluginCommandDef, PluginInfo, PluginKind,
    PluginManifest, PluginToolDef,
};
use crate::wasm::WasmPlugin;

pub struct PluginHost {
    pub plugins: Vec<Box<dyn PluginRuntime>>,
    pub errors: Vec<(String, String)>,
}

impl PluginHost {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Where plugins are installed by default, and the first place they are
    /// scanned from. The marketplace installs here so a fetched plugin is picked
    /// up by the same scan as a hand-written one.
    pub fn default_install_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".config")
            .join("pacode")
            .join("plugins")
    }

    /// The directory plugins are installed into for `config`.
    pub fn install_dir(config: &PluginsConfig) -> PathBuf {
        config
            .dirs
            .first()
            .cloned()
            .unwrap_or_else(Self::default_install_dir)
    }

    pub async fn load(config: &PluginsConfig, ui_sink: Arc<dyn UiSink>) -> Self {
        let mut host = Self::new();
        if !config.enabled {
            return host;
        }

        let scan_dirs: Vec<PathBuf> = if config.dirs.is_empty() {
            vec![Self::default_install_dir()]
        } else {
            config.dirs.clone()
        };

        for dir in scan_dirs {
            if !dir.exists() {
                continue;
            }
            if dir.join("plugin.toml").exists() {
                host.load_plugin_dir(&dir, config, ui_sink.clone()).await;
                continue;
            }
            let Ok(read_dir) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("plugin.toml").exists() {
                    host.load_plugin_dir(&path, config, ui_sink.clone()).await;
                }
            }
        }

        host
    }

    async fn load_plugin_dir(
        &mut self,
        plugin_dir: &Path,
        config: &PluginsConfig,
        ui_sink: Arc<dyn UiSink>,
    ) {
        let manifest_path = plugin_dir.join("plugin.toml");
        let manifest_str = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                self.errors.push((
                    plugin_dir.to_string_lossy().to_string(),
                    format!("failed to read plugin.toml: {e}"),
                ));
                return;
            }
        };

        let manifest: PluginManifest = match toml::from_str(&manifest_str) {
            Ok(m) => m,
            Err(e) => {
                self.errors.push((
                    plugin_dir.to_string_lossy().to_string(),
                    format!("failed to parse plugin.toml: {e}"),
                ));
                return;
            }
        };

        let entry_path = plugin_dir.join(manifest.entry_path());
        match manifest.kind {
            PluginKind::Lua => {
                match LuaPlugin::load(
                    &manifest,
                    &entry_path,
                    config.lua_memory_mb,
                    config.hook_timeout_ms,
                    ui_sink,
                ) {
                    Ok(plugin) => self.plugins.push(Box::new(plugin)),
                    Err(e) => self.errors.push((manifest.name, e.to_string())),
                }
            }
            PluginKind::Wasm => {
                match WasmPlugin::load(
                    &manifest,
                    &entry_path,
                    config.wasm_memory_mb,
                    config.hook_timeout_ms,
                    ui_sink,
                )
                .await
                {
                    Ok(plugin) => self.plugins.push(Box::new(plugin)),
                    Err(e) => self.errors.push((manifest.name, e.to_string())),
                }
            }
        }
    }

    pub fn tools(&self) -> Vec<PluginToolDef> {
        self.plugins.iter().flat_map(|p| p.tools()).collect()
    }

    pub fn commands(&self) -> Vec<PluginCommandDef> {
        self.plugins.iter().flat_map(|p| p.commands()).collect()
    }

    pub async fn call_tool(&self, name: &str, input: Value) -> Result<Value, PluginError> {
        for plugin in &self.plugins {
            if plugin.tools().iter().any(|t| t.name == name) {
                return plugin.call_tool(name, input).await;
            }
        }
        Err(PluginError::ToolNotFound {
            name: name.to_string(),
        })
    }

    pub async fn run_command(
        &self,
        name: &str,
        args: String,
    ) -> Result<CommandOutcome, PluginError> {
        for plugin in &self.plugins {
            if plugin.commands().iter().any(|c| c.name == name) {
                return plugin.run_command(name, args).await;
            }
        }
        Err(PluginError::CommandNotFound {
            name: name.to_string(),
        })
    }

    pub async fn run_hooks(&self, event: &HookEvent) -> Result<HookResult, PluginError> {
        let mut current_event = event.clone();
        let mut modified_input = None;

        for plugin in &self.plugins {
            let res = plugin.hook(current_event.clone()).await?;
            match res {
                HookResult::Deny { reason } => return Ok(HookResult::Deny { reason }),
                HookResult::ModifyInput(new_input) => {
                    match &mut current_event {
                        HookEvent::PreToolCall { input, .. } => {
                            *input = new_input.clone();
                        }
                        HookEvent::PostToolCall { input, .. } => {
                            *input = new_input.clone();
                        }
                        HookEvent::TurnStart
                        | HookEvent::TurnEnd { .. }
                        | HookEvent::OnMessage { .. } => {}
                    }
                    modified_input = Some(new_input);
                }
                HookResult::Continue => {}
            }
        }

        if let Some(final_input) = modified_input {
            Ok(HookResult::ModifyInput(final_input))
        } else {
            Ok(HookResult::Continue)
        }
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        let mut list = Vec::new();
        for p in &self.plugins {
            list.push(PluginInfo {
                name: p.name().to_string(),
                version: p.version().to_string(),
                kind: Some(p.kind()),
                description: p.description().map(str::to_string),
                tools: p.tools(),
                commands: p.commands(),
                error: None,
            });
        }
        for (name, err) in &self.errors {
            list.push(PluginInfo {
                name: name.clone(),
                version: String::new(),
                kind: None,
                description: None,
                tools: Vec::new(),
                commands: Vec::new(),
                error: Some(err.clone()),
            });
        }
        list
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}
