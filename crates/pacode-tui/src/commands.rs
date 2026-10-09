//! Slash commands typed in the prompt.
//!
//! `/model [name]` (the model palette without argument), `/login [provider]`
//! (the palette on the first provider to sign in to, without argument),
//! `/effort <low|medium|high|xhigh|max>` (picker without argument), `/mode <build|auto|plan|bypass>`, `/sessions` (picker),
//! `/mcp`, `/plugins`, `/compact`, `/clear` (reset the conversation in place),
//! `/new` (start a fresh session with the current settings),
//! `/export [path]` (write the visible transcript as markdown to `path` or `~/pacode-<session>.md`),
//! `/help`, `/quit`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::RwLock;

use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{Mode, ToastLevel};
use pacode_types::{McpServerInfo, PluginInfo, Request};

use crate::keys::Action;
use crate::state::{AppState, Focus, Overlay};

mod export;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: Cow<'static, str>,
    pub usage: Cow<'static, str>,
    pub help: Cow<'static, str>,
    pub arg_hint: Cow<'static, str>,
}

/// The cache file stores owned strings; `Cow<'static>` cannot derive
/// `Deserialize` for non-`'static` inputs, so a shadow struct does the
/// translation (every restored field lands as `Cow::Owned`).
#[derive(serde::Serialize, serde::Deserialize)]
struct SlashCommandSerde {
    name: String,
    usage: String,
    help: String,
    arg_hint: String,
}

impl serde::Serialize for SlashCommand {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        SlashCommandSerde {
            name: self.name.to_string(),
            usage: self.usage.to_string(),
            help: self.help.to_string(),
            arg_hint: self.arg_hint.to_string(),
        }
        .serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for SlashCommand {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = SlashCommandSerde::deserialize(d)?;
        Ok(SlashCommand {
            name: Cow::Owned(r.name),
            usage: Cow::Owned(r.usage),
            help: Cow::Owned(r.help),
            arg_hint: Cow::Owned(r.arg_hint),
        })
    }
}

pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: Cow::Borrowed("model"),
        usage: Cow::Borrowed("/model"),
        help: Cow::Borrowed("model palette: model, effort, sign-in"),
        arg_hint: Cow::Borrowed("[provider/model]"),
    },
    SlashCommand {
        name: Cow::Borrowed("effort"),
        usage: Cow::Borrowed("/effort"),
        help: Cow::Borrowed("reasoning effort"),
        arg_hint: Cow::Borrowed("[low|medium|high|xhigh|max]"),
    },
    SlashCommand {
        name: Cow::Borrowed("mode"),
        usage: Cow::Borrowed("/mode"),
        help: Cow::Borrowed("permission mode"),
        arg_hint: Cow::Borrowed("[build|auto|plan|bypass]"),
    },
    SlashCommand {
        name: Cow::Borrowed("theme"),
        usage: Cow::Borrowed("/theme"),
        help: Cow::Borrowed("switch color theme"),
        arg_hint: Cow::Borrowed("[name]"),
    },
    SlashCommand {
        name: Cow::Borrowed("menu"),
        usage: Cow::Borrowed("/menu"),
        help: Cow::Borrowed("open the menu hub"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("config"),
        usage: Cow::Borrowed("/config"),
        help: Cow::Borrowed("settings"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("settings"),
        usage: Cow::Borrowed("/settings"),
        help: Cow::Borrowed("settings — alias of /config"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("sessions"),
        usage: Cow::Borrowed("/sessions"),
        help: Cow::Borrowed("pick a session"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("login"),
        usage: Cow::Borrowed("/login"),
        help: Cow::Borrowed("sign in to a model provider"),
        arg_hint: Cow::Borrowed("[provider]"),
    },
    SlashCommand {
        name: Cow::Borrowed("mcp"),
        usage: Cow::Borrowed("/mcp"),
        help: Cow::Borrowed("manage MCP servers"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("plugins"),
        usage: Cow::Borrowed("/plugins"),
        help: Cow::Borrowed("manage plugins"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("keys"),
        usage: Cow::Borrowed("/keys"),
        help: Cow::Borrowed("customize keybindings"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("editor"),
        usage: Cow::Borrowed("/editor"),
        help: Cow::Borrowed("edit prompt in $EDITOR"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("import"),
        usage: Cow::Borrowed("/import"),
        help: Cow::Borrowed("import MCP servers and skills"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("analyze"),
        usage: Cow::Borrowed("/analyze"),
        help: Cow::Borrowed("where the tokens went, and whether the context held up"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("compact"),
        usage: Cow::Borrowed("/compact"),
        help: Cow::Borrowed("compact the context now"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("clear"),
        usage: Cow::Borrowed("/clear"),
        help: Cow::Borrowed("forget the conversation, keep the session"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("new"),
        usage: Cow::Borrowed("/new"),
        help: Cow::Borrowed("start a fresh session; this one stays in /sessions"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("export"),
        usage: Cow::Borrowed("/export [path]"),
        help: Cow::Borrowed("save transcript as markdown"),
        arg_hint: Cow::Borrowed("[path]"),
    },
    SlashCommand {
        name: Cow::Borrowed("help"),
        usage: Cow::Borrowed("/help"),
        help: Cow::Borrowed("show keys and commands"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("quit"),
        usage: Cow::Borrowed("/quit"),
        help: Cow::Borrowed("exit the client"),
        arg_hint: Cow::Borrowed(""),
    },
];

static DYNAMIC_COMMANDS: RwLock<Vec<SlashCommand>> = RwLock::new(Vec::new());

/// Returns all static and dynamically registered commands.
pub fn all_commands() -> Vec<SlashCommand> {
    let mut list = COMMANDS.to_vec();
    if let Ok(guard) = DYNAMIC_COMMANDS.read() {
        list.extend(guard.iter().cloned());
    }
    list
}

/// Commands whose name starts with `prefix` (for the popup).
pub fn matching(prefix: &str) -> Vec<SlashCommand> {
    all_commands()
        .into_iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect()
}

/// Find a command by exact name.
pub fn find_command(name: &str) -> Option<SlashCommand> {
    all_commands().into_iter().find(|c| c.name == name)
}

/// Register an MCP prompt slash command `/mcp:<server>:<prompt>`.
pub fn register_mcp_prompt(server: &str, prompt: &str) {
    let name = format!("mcp:{server}:{prompt}");
    let usage = format!("/mcp:{server}:{prompt}");
    let help = format!("MCP prompt '{prompt}' from {server}");
    let cmd = SlashCommand {
        name: Cow::Owned(name),
        usage: Cow::Owned(usage),
        help: Cow::Owned(help),
        arg_hint: Cow::Borrowed("[key=value ...]"),
    };
    if let Ok(mut guard) = DYNAMIC_COMMANDS.write() {
        if let Some(pos) = guard.iter().position(|c| c.name == cmd.name) {
            guard[pos] = cmd;
        } else {
            guard.push(cmd);
        }
    }
}

/// Register all MCP prompts from a list of MCP server info structs.
pub fn register_mcp_servers(servers: &[McpServerInfo]) {
    for s in servers {
        for prompt in &s.prompt_names {
            register_mcp_prompt(&s.name, prompt);
        }
    }
}

/// Register a plugin slash command `/<name>`.
pub fn register_plugin_command(name: &str) {
    let usage = format!("/{name}");
    let help = format!("run plugin command '{name}'");
    let cmd = SlashCommand {
        name: Cow::Owned(name.to_string()),
        usage: Cow::Owned(usage),
        help: Cow::Owned(help),
        arg_hint: Cow::Borrowed("[args]"),
    };
    if let Ok(mut guard) = DYNAMIC_COMMANDS.write() {
        if let Some(pos) = guard.iter().position(|c| c.name == cmd.name) {
            guard[pos] = cmd;
        } else {
            guard.push(cmd);
        }
    }
}

/// Register all commands from a list of plugins.
pub fn register_plugins(plugins: &[PluginInfo]) {
    for p in plugins {
        for cmd in &p.commands {
            register_plugin_command(cmd);
        }
    }
}

/// Clear dynamic commands (primarily for tests).
pub fn clear_dynamic_commands() {
    if let Ok(mut guard) = DYNAMIC_COMMANDS.write() {
        guard.clear();
    }
}

/// The current dynamic registry — written to the `commands` cache file so the
/// next run completes `/x` before the daemon answers (§8.2).
pub fn dynamic_commands() -> Vec<SlashCommand> {
    DYNAMIC_COMMANDS
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Load the registry from the disk snapshot, wholesale — the file is the
/// registry, not a merge source.
pub fn restore_dynamic_commands(cmds: Vec<SlashCommand>) {
    if let Ok(mut guard) = DYNAMIC_COMMANDS.write() {
        *guard = cmds;
    }
}

/// Rebuild the registry from fresh lists. Replacing (not merging) is what
/// makes a removed plugin's commands disappear (§8.2: fresh data replaces
/// cached state wholesale).
pub fn rebuild_dynamic_commands(plugins: &[PluginInfo], servers: &[McpServerInfo]) {
    clear_dynamic_commands();
    register_plugins(plugins);
    register_mcp_servers(servers);
}

/// Check if a command name is a registered plugin command.
pub fn is_plugin_command(state: &AppState, name: &str) -> bool {
    state
        .plugins
        .iter()
        .any(|p| p.commands.iter().any(|c| c == name))
        || DYNAMIC_COMMANDS
            .read()
            .map(|g| {
                g.iter()
                    .any(|c| c.name == name && !c.name.starts_with("mcp:"))
            })
            .unwrap_or(false)
}

/// Execute a submitted `/command args` line. Unknown commands become a Notice.
pub fn execute(state: &mut AppState, line: &str) -> Vec<Action> {
    let trimmed = line.trim();
    let without_slash = trimmed.strip_prefix('/').unwrap_or(trimmed);
    let mut parts = without_slash.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.collect::<Vec<_>>().join(" ");

    match cmd {
        "login" => {
            if arg.is_empty() {
                log::debug!("open overlay: model palette (login)");
                crate::keys_palette::open(state, crate::keys_palette::Opening::Login)
            } else {
                vec![Action::Send(Request::Login { provider: arg })]
            }
        }
        "model" => {
            if arg.is_empty() {
                open_model_picker(state)
            } else {
                let route = if let Some((p, m)) = arg.split_once('/') {
                    ModelRoute::new(p, m)
                } else {
                    ModelRoute::new("default", &arg)
                };
                // Downgrade the session effort if the new route can't run it
                // (spec §6) — after the switch, and only if it succeeded.
                let effort = state.clamp_effort(&route);
                state.model_switch_actions(route, effort, false)
            }
        }
        "effort" => {
            if arg.is_empty() {
                log::debug!("open overlay: EffortPicker");
                let cur_idx = Effort::ALL
                    .iter()
                    .position(|e| *e == state.effort())
                    .unwrap_or(1);
                state.focus = Focus::Overlay(Overlay::EffortPicker { index: cur_idx });
                state.dirty = true;
                vec![]
            } else if let Some(eff) = Effort::parse(&arg) {
                if !state.effort_supported(eff) {
                    push_notice(
                        state,
                        ToastLevel::Warn,
                        format!("effort {eff} is not supported by the current model"),
                    );
                    vec![]
                } else {
                    vec![Action::Send(Request::SetEffort(eff))]
                }
            } else {
                push_notice(
                    state,
                    ToastLevel::Warn,
                    format!(
                        "Invalid effort '{arg}'. Valid: {}",
                        Effort::ALL.map(|e| e.to_string()).join(", ")
                    ),
                );
                vec![]
            }
        }
        "mode" => {
            if arg.is_empty() {
                log::debug!("open overlay: ModePicker");
                let cur_idx = Mode::CYCLE
                    .iter()
                    .position(|m| *m == state.mode())
                    .unwrap_or(0);
                state.focus = Focus::Overlay(Overlay::ModePicker { index: cur_idx });
                state.dirty = true;
                vec![]
            } else if let Some(m) = Mode::parse(&arg) {
                vec![Action::Send(Request::SetMode(m))]
            } else {
                push_notice(
                    state,
                    ToastLevel::Warn,
                    format!("Invalid mode '{arg}'. Valid: build, auto, plan, bypass"),
                );
                vec![]
            }
        }
        "theme" => {
            let truecolor = state.truecolor;
            if arg.is_empty() {
                open_theme_picker(state)
            } else if !pacode_render::builtin_palettes()
                .iter()
                .any(|p| p.name == arg)
                && !pacode_config::user_theme_names(&state.paths).contains(&arg)
            {
                // `load_theme` would quietly fall back to the default, and the
                // typo would be saved as the configured theme.
                push_notice(
                    state,
                    ToastLevel::Warn,
                    format!("Unknown theme '{arg}'. Run /theme to pick one."),
                );
                vec![]
            } else {
                let theme_cfg = pacode_types::ThemeConfig {
                    name: arg.clone(),
                    overrides: state.config.theme.overrides.clone(),
                };
                let (palette, warnings) = pacode_config::load_theme(&state.paths, &theme_cfg);
                state.theme = pacode_render::Theme::from_palette(&palette, truecolor);
                state.config.theme.name = arg.clone();
                state.sync_render_context();
                if let Err(e) = pacode_config::update_config_value(
                    &state.paths,
                    "theme.name",
                    pacode_config::toml::Value::String(arg.clone()),
                ) {
                    log::warn!("failed to update config value 'theme.name': {e}");
                }
                for w in warnings {
                    push_notice(state, ToastLevel::Warn, w);
                }
                push_notice(state, ToastLevel::Success, format!("Theme set to {arg}"));
                vec![]
            }
        }
        "menu" => {
            log::debug!("open overlay: menu hub");
            crate::keys_menu::open(state, crate::ui::menu::MenuId::Root)
        }
        "config" | "settings" => {
            // The full page, not the hub's level: the hub row is a shortcut into
            // the same settings, and two settings screens is one too many.
            log::debug!("open overlay: settings page");
            state.focus = Focus::Overlay(Overlay::Settings {
                ui: crate::ui::settings::SettingsState::new(),
            });
            state.dirty = true;
            vec![]
        }
        "sessions" => {
            log::debug!("open menu: sessions");
            crate::keys_menu::open(state, crate::ui::menu::MenuId::Sessions)
        }
        "mcp" => {
            log::debug!("open menu: mcp");
            crate::keys_menu::open(state, crate::ui::menu::MenuId::Mcp)
        }
        "plugins" => {
            log::debug!("open menu: plugins");
            // `/plugins <source>` points the Discover tab at a marketplace and
            // remembers it; `/plugins` alone uses the configured one.
            let source = arg.trim();
            if !source.is_empty() {
                state.config.plugins.marketplace = source.to_string();
                state.save_pref_marketplace(source);
            }
            let actions = crate::keys_menu::open(state, crate::ui::menu::MenuId::Plugins);
            // A non-empty source lands on Discover.
            if !source.is_empty()
                && let Focus::Overlay(Overlay::Menu { ui, .. }) = &mut state.focus
            {
                ui.tab = crate::ui::menu::plugins::TAB_DISCOVER;
            }
            actions
        }
        "keys" => {
            log::debug!("open menu: keys");
            crate::keys_menu::open(state, crate::ui::menu::MenuId::Keys)
        }
        "editor" => {
            crate::editor::open_editor(state);
            vec![]
        }
        "import" => {
            log::debug!("open menu: import");
            crate::keys_menu::open(state, crate::ui::menu::MenuId::Import)
        }
        "analyze" => {
            // Opens the page and asks for the reading in the same breath: the
            // overlay is the answer, and an overlay with nothing in it is a
            // frame of "loading" that never resolves.
            state.open_analyze();
            if state.analyze_mode {
                return vec![Action::Send(Request::Analyze)];
            }
            vec![]
        }
        "compact" => vec![Action::Send(Request::Compact)],
        // The view resets on `Event::ContextCleared`, not here: a refused
        // clear must leave the transcript as it was, next to the error.
        "clear" => vec![Action::Send(Request::ClearContext)],
        "new" => new_session(state),
        "export" => export::run(state, &arg),
        "help" => {
            log::debug!("open overlay: Help");
            crate::keys_menu::open(state, crate::ui::menu::MenuId::Help)
        }
        "quit" => vec![Action::Quit],
        other if other.starts_with("mcp:") => {
            let parts: Vec<&str> = other.splitn(3, ':').collect();
            if parts.len() == 3 {
                let server = parts[1].to_string();
                let prompt = parts[2].to_string();
                let mut args = BTreeMap::new();
                for token in arg.split_whitespace() {
                    if let Some((k, v)) = token.split_once('=') {
                        args.insert(k.to_string(), v.to_string());
                    }
                }
                vec![Action::Send(Request::GetMcpPrompt {
                    server,
                    name: prompt,
                    args,
                })]
            } else {
                push_notice(
                    state,
                    ToastLevel::Warn,
                    format!("Invalid MCP prompt format: /{other}. Expected /mcp:<server>:<prompt>"),
                );
                vec![]
            }
        }
        other if is_plugin_command(state, other) => {
            vec![Action::Send(Request::RunPluginCommand {
                name: other.to_string(),
                args: arg,
            })]
        }
        _ => {
            push_notice(
                state,
                ToastLevel::Warn,
                format!("Unknown command: /{cmd}. Type /help for available commands."),
            );
            vec![]
        }
    }
}

/// `/new` — a fresh session on the current route, effort and mode, attached
/// in place of this one. The old session is detached, not closed: a turn
/// running there keeps running, and `/sessions` resumes it.
fn new_session(state: &mut AppState) -> Vec<Action> {
    let attach = pacode_types::Attach::New {
        cwd: state.cwd(),
        model: state.meta.as_ref().map(|meta| meta.model.clone()),
        effort: state.meta.as_ref().map(|meta| meta.effort),
        mode: state.meta.as_ref().map(|meta| meta.mode),
    };
    log::debug!("new session from /new");
    state.leave_active_slot();
    state.begin_attach(attach.clone());
    vec![Action::SwitchSession { attach }]
}

/// `/model` with no argument, and the hub's model row: the central model
/// palette, landing on the session's route.
pub fn open_model_picker(state: &mut AppState) -> Vec<Action> {
    log::debug!("open overlay: model palette");
    crate::keys_palette::open(state, crate::keys_palette::Opening::Model)
}

/// `/theme` with no argument — the bottom picker, pre-selected on the active
/// theme. Also the hub's `Theme` row (`ui/menu/root.rs`).
pub fn open_theme_picker(state: &mut AppState) -> Vec<Action> {
    log::debug!("open overlay: ThemePicker");
    let user_themes = pacode_config::user_theme_names(&state.paths);
    let builtins = pacode_render::builtin_palettes();
    let current_name = state.config.theme.name.clone();
    let index = builtins
        .iter()
        .position(|p| p.name == current_name)
        .or_else(|| {
            user_themes
                .iter()
                .position(|u| u == &current_name)
                .map(|i| builtins.len() + i)
        })
        .unwrap_or(0);

    state.focus = Focus::Overlay(Overlay::ThemePicker {
        index,
        original_theme: Box::new(state.theme.clone()),
        original_name: current_name,
        step: crate::state::ThemePickerStep::SelectTheme,
        user_themes,
    });
    state.dirty = true;
    vec![]
}

fn push_notice(state: &mut AppState, level: ToastLevel, text: String) {
    state.push_notice_with_level(level, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commands_arg_hints() {
        let login = COMMANDS.iter().find(|c| c.name == "login").unwrap();
        assert_eq!(login.arg_hint, "[provider]");

        let effort = COMMANDS.iter().find(|c| c.name == "effort").unwrap();
        assert_eq!(effort.arg_hint, "[low|medium|high|xhigh|max]");

        let mode = COMMANDS.iter().find(|c| c.name == "mode").unwrap();
        assert_eq!(mode.arg_hint, "[build|auto|plan|bypass]");

        let model = COMMANDS.iter().find(|c| c.name == "model").unwrap();
        assert_eq!(model.arg_hint, "[provider/model]");

        let export = COMMANDS.iter().find(|c| c.name == "export").unwrap();
        assert_eq!(export.arg_hint, "[path]");

        let config = COMMANDS.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(config.arg_hint, "");

        let mcp = COMMANDS.iter().find(|c| c.name == "mcp").unwrap();
        assert_eq!(mcp.arg_hint, "");

        let plugins = COMMANDS.iter().find(|c| c.name == "plugins").unwrap();
        assert_eq!(plugins.arg_hint, "");

        let keys = COMMANDS.iter().find(|c| c.name == "keys").unwrap();
        assert_eq!(keys.arg_hint, "");

        let editor = COMMANDS.iter().find(|c| c.name == "editor").unwrap();
        assert_eq!(editor.arg_hint, "");

        let import = COMMANDS.iter().find(|c| c.name == "import").unwrap();
        assert_eq!(import.arg_hint, "");

        let theme = COMMANDS.iter().find(|c| c.name == "theme").unwrap();
        assert_eq!(theme.arg_hint, "[name]");
    }
}
