//! Slash commands typed in the prompt.
//!
//! `/model [name]` (picker without argument), `/effort <low|medium|high|xhigh|max>` (picker
//! without argument), `/mode <build|auto|plan|bypass>`, `/sessions` (picker),
//! `/compact`, `/clear` (transcript view only), `/export [path]` (write the visible
//! transcript as markdown to `path` or `~/pacode-<session>.md`), `/help`, `/quit`.

use pacode_types::model::{Effort, ModelRoute};
use pacode_types::state::{Mode, ToastLevel};
use pacode_types::time::now_ms;
use pacode_types::{Request, TranscriptKind};

use crate::keys::Action;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{AppState, Focus, Overlay};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub usage: &'static str,
    pub help: &'static str,
    pub arg_hint: &'static str,
}

pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "model",
        usage: "/model",
        help: "switch model",
        arg_hint: "[provider/model]",
    },
    SlashCommand {
        name: "effort",
        usage: "/effort",
        help: "reasoning effort",
        arg_hint: "[low|medium|high|xhigh|max]",
    },
    SlashCommand {
        name: "mode",
        usage: "/mode",
        help: "permission mode",
        arg_hint: "[build|auto|plan|bypass]",
    },
    SlashCommand {
        name: "config",
        usage: "/config",
        help: "quick settings",
        arg_hint: "",
    },
    SlashCommand {
        name: "sessions",
        usage: "/sessions",
        help: "pick a session",
        arg_hint: "",
    },
    SlashCommand {
        name: "compact",
        usage: "/compact",
        help: "compact the context now",
        arg_hint: "",
    },
    SlashCommand {
        name: "clear",
        usage: "/clear",
        help: "clear the transcript view",
        arg_hint: "",
    },
    SlashCommand {
        name: "export",
        usage: "/export [path]",
        help: "save transcript as markdown",
        arg_hint: "[path]",
    },
    SlashCommand {
        name: "help",
        usage: "/help",
        help: "show keys and commands",
        arg_hint: "",
    },
    SlashCommand {
        name: "quit",
        usage: "/quit",
        help: "exit the client",
        arg_hint: "",
    },
];

/// Commands whose name starts with `prefix` (for the popup).
pub fn matching(prefix: &str) -> Vec<&'static SlashCommand> {
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect()
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
    }
}
