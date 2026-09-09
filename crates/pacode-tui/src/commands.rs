//! Slash commands typed in the prompt.
//!
//! `/model [name]` (picker without argument), `/effort <low|medium|high|xhigh|max>` (picker
//! without argument), `/mode <build|auto|plan|bypass>`, `/sessions` (picker),
//! `/mcp`, `/plugins`, `/compact`, `/clear` (transcript view only),
//! `/export [path]` (write the visible transcript as markdown to `path` or `~/pacode-<session>.md`),
//! `/help`, `/quit`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::RwLock;

use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{Mode, ToastLevel};
use pacode_types::time::now_ms;
use pacode_types::{McpServerInfo, PluginInfo, Request, TranscriptKind};

use crate::keys::Action;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{AppState, Focus, Overlay};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: Cow<'static, str>,
    pub usage: Cow<'static, str>,
    pub help: Cow<'static, str>,
    pub arg_hint: Cow<'static, str>,
}

pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: Cow::Borrowed("model"),
        usage: Cow::Borrowed("/model"),
        help: Cow::Borrowed("switch model"),
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
        name: Cow::Borrowed("config"),
        usage: Cow::Borrowed("/config"),
        help: Cow::Borrowed("quick settings"),
        arg_hint: Cow::Borrowed(""),
    },
    SlashCommand {
        name: Cow::Borrowed("sessions"),
        usage: Cow::Borrowed("/sessions"),
        help: Cow::Borrowed("pick a session"),
        arg_hint: Cow::Borrowed(""),
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
        name: Cow::Borrowed("import"),
        usage: Cow::Borrowed("/import"),
        help: Cow::Borrowed("import MCP servers and skills"),
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
        help: Cow::Borrowed("clear the transcript view"),
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
        "model" => {
            if arg.is_empty() {
                state.focus = Focus::Overlay(Overlay::ModelPicker {
                    query: String::new(),
                    index: 0,
                });
                state.dirty = true;
                vec![Action::Send(Request::ListModels)]
            } else {
                let route = if let Some((p, m)) = arg.split_once('/') {
                    ModelRoute::new(p, m)
                } else {
                    ModelRoute::new("default", &arg)
                };
                state.save_pref_model(&route.to_string());
                vec![Action::Send(Request::SetModel(route))]
            }
        }
        "effort" => {
            if arg.is_empty() {
                let cur_idx = Effort::ALL
                    .iter()
                    .position(|e| *e == state.effort())
                    .unwrap_or(1);
                state.focus = Focus::Overlay(Overlay::EffortPicker { index: cur_idx });
                state.dirty = true;
                vec![]
            } else if let Some(eff) = Effort::parse(&arg) {
                state.save_pref_effort(eff);
                vec![Action::Send(Request::SetEffort(eff))]
            } else {
                push_notice(
                    state,
                    ToastLevel::Warn,
                    format!("Invalid effort '{arg}'. Valid: low, medium, high, max"),
                );
                vec![]
            }
        }
        "mode" => {
            if arg.is_empty() {
                let cur_idx = Mode::CYCLE
                    .iter()
                    .position(|m| *m == state.mode())
                    .unwrap_or(0);
                state.focus = Focus::Overlay(Overlay::ModePicker { index: cur_idx });
                state.dirty = true;
                vec![]
            } else if let Some(m) = Mode::parse(&arg) {
                state.save_pref_mode(m);
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
        "config" => {
            state.focus = Focus::Overlay(Overlay::ConfigPicker {
                index: 0,
                editing_number: None,
            });
            state.dirty = true;
            vec![]
        }
        "sessions" => {
            state.focus = Focus::Overlay(Overlay::SessionPicker {
                query: String::new(),
                index: 0,
            });
            state.dirty = true;
            vec![Action::Send(Request::ListSessions { limit: 50 })]
        }
        "mcp" => {
            state.focus = Focus::Overlay(Overlay::McpPicker {
                index: 0,
                servers: Vec::new(),
                loading: true,
            });
            state.dirty = true;
            vec![Action::Send(Request::ListMcpServers)]
        }
        "plugins" => {
            state.focus = Focus::Overlay(Overlay::PluginsPicker {
                index: 0,
                plugins: state.plugins.clone(),
            });
            state.dirty = true;
            vec![Action::Send(Request::ListPlugins)]
        }
        "import" => {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let overlay_state =
                crate::ui::import::ImportOverlayState::discover(&home, &cwd, &state.paths);
            state.focus = Focus::Overlay(Overlay::Import(overlay_state));
            state.dirty = true;
            vec![]
        }
        "compact" => vec![Action::Send(Request::Compact)],
        "clear" => {
            state.transcript.cells.clear();
            state.transcript.cache.clear();
            state.dirty = true;
            vec![]
        }
        "export" => {
            let path = if arg.is_empty() {
                let ses_id = state
                    .meta
                    .as_ref()
                    .map(|m| m.id.to_string())
                    .unwrap_or_else(|| "current".to_string());
                format!("pacode-{ses_id}.md")
            } else {
                arg
            };

            let mut md = String::new();
            for cell in &state.transcript.cells {
                match &cell.kind {
                    CellKind::Item(TranscriptKind::User { text }) => {
                        md.push_str(&format!("### User\n\n{text}\n\n"));
                    }
                    CellKind::Item(TranscriptKind::Assistant { text, .. }) => {
                        md.push_str(&format!("### Assistant\n\n{text}\n\n"));
                    }
                    CellKind::Item(TranscriptKind::ToolCall { name, title, .. }) => {
                        md.push_str(&format!("*Tool: {name} - {title}*\n\n"));
                    }
                    _ => {}
                }
            }

            match std::fs::write(&path, md) {
                Ok(_) => {
                    push_notice(state, ToastLevel::Success, format!("Exported to {path}"));
                }
                Err(e) => {
                    push_notice(state, ToastLevel::Error, format!("Export failed: {e}"));
                }
            }
            vec![]
        }
        "help" => {
            state.focus = Focus::Overlay(Overlay::Help);
            state.dirty = true;
            vec![]
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

fn push_notice(state: &mut AppState, level: ToastLevel, text: String) {
    let now = now_ms();
    state.transcript.cells.push_back(Cell {
        id: now,
        kind: CellKind::Item(TranscriptKind::Notice { level, text }),
        version: 0,
        ts_ms: now,
        stats: None,
    });
    state.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commands_arg_hints() {
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

        let import = COMMANDS.iter().find(|c| c.name == "import").unwrap();
        assert_eq!(import.arg_hint, "");
    }
}
