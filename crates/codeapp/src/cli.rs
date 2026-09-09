//! clap definitions and command dispatch.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "codeapp", version, about = "background-first coding agent")]
pub struct Cli {
    /// Prompt to send immediately.
    pub prompt: Option<String>,
    #[arg(long)]
    pub resume: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long)]
    pub mode: Option<String>,
    #[arg(short = 'C', long)]
    pub dir: Option<PathBuf>,
    #[arg(long)]
    pub socket: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the daemon.
    Serve {
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Headless prompt.
    Run {
        prompt: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(short = 'C', long)]
        dir: Option<PathBuf>,
    },
    /// List or delete sessions.
    Sessions {
        #[command(subcommand)]
        action: Option<SessionsAction>,
    },
    /// Daemon status / stop.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum SessionsAction {
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Delete {
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    Status,
    Stop {
        #[arg(long)]
        force: bool,
    },
}

pub fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let _ = cli;
    todo!("cli::main")
}
