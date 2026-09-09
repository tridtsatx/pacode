//! Slash commands typed in the prompt.
//!
//! `/model [name]` (picker without argument), `/effort <low|medium|high|max>` (picker
//! without argument), `/mode <build|auto|plan|bypass>`, `/sessions` (picker),
//! `/compact`, `/clear` (transcript view only), `/export [path]` (write the visible
//! transcript as markdown to `path` or `~/codeapp-<session>.md`), `/help`, `/quit`.

use codeapp_types::Request;

use crate::keys::Action;
use crate::state::AppState;

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
    let _ = (state, line);
    let _: Option<Request> = None;
    todo!("commands::execute")
}
