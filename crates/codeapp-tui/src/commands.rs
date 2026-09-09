//! Slash commands typed in the prompt.
//!
//! `/model [name]` (picker without argument), `/effort <low|medium|high|max>` (picker
//! without argument), `/mode <build|auto|plan|bypass>`, `/sessions` (picker),
//! `/compact`, `/clear` (transcript view only), `/export [path]` (write the visible
//! transcript as markdown to `path` or `~/codeapp-<session>.md`), `/help`, `/quit`.

use codeapp_types::model::{Effort, ModelRoute};
use codeapp_types::state::{Mode, ToastLevel};
use codeapp_types::time::now_ms;
use codeapp_types::{Request, TranscriptKind};

use crate::keys::Action;
use crate::state::transcript::{Cell, CellKind};
use crate::state::{AppState, Focus, Overlay};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub usage: &'static str,
    pub help: &'static str,
}

pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "model",
        usage: "/model [provider/model]",
        help: "switch model",
    },
    SlashCommand {
        name: "effort",
        usage: "/effort [low|medium|high|max]",
        help: "reasoning effort",
    },
    SlashCommand {
        name: "mode",
        usage: "/mode [build|auto|plan|bypass]",
        help: "permission mode",
    },
    SlashCommand {
        name: "sessions",
        usage: "/sessions",
        help: "pick a session",
    },
    SlashCommand {
        name: "compact",
        usage: "/compact",
        help: "compact the context now",
    },
    SlashCommand {
        name: "clear",
        usage: "/clear",
        help: "clear the transcript view",
    },
    SlashCommand {
        name: "export",
        usage: "/export [path]",
        help: "save transcript as markdown",
    },
    SlashCommand {
        name: "help",
        usage: "/help",
        help: "show keys and commands",
    },
    SlashCommand {
        name: "quit",
        usage: "/quit",
        help: "exit the client",
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
                vec![Action::Send(Request::SetModel(route))]
            }
        }
        "effort" => {
            if arg.is_empty() {
                state.focus = Focus::Overlay(Overlay::EffortPicker { index: 0 });
                state.dirty = true;
                vec![]
            } else if let Some(eff) = Effort::parse(&arg) {
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
                let next = state.mode().next();
                vec![Action::Send(Request::SetMode(next))]
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
                format!("codeapp-{ses_id}.md")
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
    });
    state.dirty = true;
}
