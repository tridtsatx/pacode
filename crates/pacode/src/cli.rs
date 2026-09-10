//! clap definitions and command dispatch.

use std::path::PathBuf;

use anyhow::Context;
use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use pacode_client::ClientOptions;
use pacode_config::Paths;
use pacode_types::{Attach, Config, Effort, Mode, ModelRoute, SessionId};

#[path = "daemon.rs"]
mod daemon;
#[path = "import_cmd.rs"]
mod import_cmd;
#[path = "login_cmd.rs"]
mod login_cmd;
#[path = "mcp_cmd.rs"]
mod mcp_cmd;
#[path = "plugins_cmd.rs"]
mod plugins_cmd;
#[path = "run.rs"]
mod run;
#[path = "serve.rs"]
mod serve;
#[path = "sessions.rs"]
mod sessions;

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;

pub const HELP_TEMPLATE: &str = "\x20▄▄▄▄▄
█ ▀ ██
███▀
 ▀▀▀▀▀
pacode v{version} — a coding agent that eats your backlog

{usage-heading}
  {usage}

{all-args}{after-help}
";

pub fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .usage(Effects::BOLD.into())
        .literal(AnsiColor::Cyan.on_default())
        .placeholder(Effects::DIMMED.into())
}

#[derive(Parser, Debug)]
#[command(
    name = "pacode",
    version,
    about = "a coding agent that eats your backlog",
    styles = cli_styles(),
    help_template = HELP_TEMPLATE,
)]
pub struct Cli {
    /// Prompt to send immediately upon launch.
    pub prompt: Option<String>,

    /// Resume an existing session by ID.
    #[arg(short = 's', long = "session", alias = "resume", value_name = "ID")]
    pub session: Option<String>,

    /// Model override in provider/model format (e.g. bubna/gemini-3.8-flash).
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Reasoning effort override (low, medium, high, max).
    #[arg(long, value_name = "EFFORT")]
    pub effort: Option<String>,

    /// Permission mode override (build, auto, plan, bypass).
    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,

    /// Working directory for the session.
    #[arg(short = 'C', long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Path to the daemon Unix domain socket.
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Subcommand to execute instead of launching the interactive TUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start the background daemon server to manage sessions and agents.
    Serve {
        /// Detach and run as a background daemon process.
        #[arg(long)]
        detach: bool,

        /// Path to listen on for client connections.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
    },

    /// Execute a prompt in headless non-interactive mode without launching the TUI.
    Run {
        /// Prompt string to execute.
        prompt: String,

        /// Output stream events as newline-delimited JSON.
        #[arg(long)]
        json: bool,

        /// Model override in provider/model format.
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,

        /// Reasoning effort override (low, medium, high, max).
        #[arg(long, value_name = "EFFORT")]
        effort: Option<String>,

        /// Permission mode override (build, auto, plan, bypass).
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,

        /// Working directory for execution.
        #[arg(short = 'C', long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },

    /// List or delete recorded sessions.
    Sessions {
        /// Session action to perform (defaults to listing sessions).
        #[command(subcommand)]
        action: Option<SessionsAction>,
    },

    /// Manage Model Context Protocol (MCP) servers.
    Mcp {
        /// MCP action to perform (defaults to listing servers).
        #[command(subcommand)]
        action: Option<McpAction>,
    },

    /// Manage plugins.
    Plugins {
        /// Plugin action to perform (defaults to listing plugins).
        #[command(subcommand)]
        action: Option<PluginsAction>,
    },

    /// Import MCP servers and skills from other coding agents.
    Import {
        /// Sources to import from (claude, codex, opencode, cursor, gemini, vscode, all).
        #[arg(long = "from", value_name = "SOURCE")]
        from: Vec<String>,

        /// Import MCP servers only.
        #[arg(long)]
        mcp: bool,

        /// Import skills only.
        #[arg(long)]
        skills: bool,

        /// Apply changes (default is dry-run).
        #[arg(long)]
        apply: bool,

        /// Overwrite existing servers or skill directories on conflict.
        #[arg(long)]
        force: bool,
    },

    /// Sign in to a model provider.
    Login {
        /// Provider identifier to sign in to (e.g. devin, anthropic, openai).
        #[arg(long, value_name = "ID")]
        provider: Option<String>,

        /// List available providers and their status.
        #[arg(long)]
        list: bool,
    },

    /// Query status or request shutdown of the background daemon.
    Daemon {
        /// Daemon action to perform.
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Run as an ACP (Agent Client Protocol) agent over stdio.
    #[cfg(feature = "acp")]
    Acp,
}

#[derive(Subcommand, Debug)]
pub enum SessionsAction {
    /// List recently active sessions.
    List {
        /// Maximum number of sessions to display.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },

    /// Delete a session and its saved state by ID.
    Delete {
        /// Identifier of the session to delete.
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Check if the daemon is currently running and responsive.
    Status,

    /// Request the daemon to shut down cleanly.
    Stop {
        /// Force immediate shutdown even if sessions are active.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum McpAction {
    /// List configured MCP servers.
    List,

    /// Add or update an MCP server configuration.
    Add {
        /// Name of the MCP server.
        name: String,

        /// Command and arguments to run (for stdio transport).
        #[arg(long, conflicts_with = "url")]
        cmd: Option<String>,

        /// SSE / HTTP endpoint URL (for http transport).
        #[arg(long, conflicts_with = "cmd")]
        url: Option<String>,

        /// HTTP header in K=V format (repeatable, for http transport).
        #[arg(long = "header", value_name = "K=V")]
        headers: Vec<String>,

        /// Environment variable in K=V format (repeatable, for stdio transport).
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,

        /// Start server on first tool call instead of session start.
        #[arg(long, overrides_with = "no_lazy")]
        lazy: bool,

        /// Start server immediately at session start.
        #[arg(long = "no-lazy", overrides_with = "lazy")]
        no_lazy: bool,
    },

    /// Remove an MCP server configuration.
    Remove {
        /// Name of the MCP server to remove.
        name: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum PluginsAction {
    /// List loaded plugins.
    List,
}

pub fn build_attach(
    session: Option<String>,
    cwd: PathBuf,
    model: Option<ModelRoute>,
    effort: Option<Effort>,
    mode: Option<Mode>,
) -> Attach {
    match session {
        Some(id) if !id.trim().is_empty() => Attach::Resume {
            session: SessionId::new(id.trim()),
        },
        _ => Attach::New {
            cwd,
            model,
            effort,
            mode,
        },
    }
}

pub fn parse_model_override(
    raw: Option<&str>,
    config: &Config,
) -> anyhow::Result<Option<ModelRoute>> {
    let Some(s) = raw else {
        return Ok(None);
    };
    let default_provider = config
        .default_route()
        .map(|r| r.provider)
        .or_else(|| config.providers.keys().next().cloned());
    let known = config.providers.keys().map(String::as_str);
    let route = ModelRoute::parse(s, known, default_provider.as_deref())
        .or_else(|| ModelRoute::parse_lossy(s))
        .ok_or_else(|| anyhow::anyhow!("invalid model: {s}"))?;
    Ok(Some(route))
}

pub fn parse_effort_override(raw: Option<&str>) -> anyhow::Result<Option<Effort>> {
    let Some(s) = raw else {
        return Ok(None);
    };
    let effort = Effort::parse(s).ok_or_else(|| {
        anyhow::anyhow!("invalid effort level: {s} (expected low, medium, high, max)")
    })?;
    Ok(Some(effort))
}

pub fn parse_mode_override(raw: Option<&str>) -> anyhow::Result<Option<Mode>> {
    let Some(s) = raw else {
        return Ok(None);
    };
    let mode = Mode::parse(s)
        .ok_or_else(|| anyhow::anyhow!("invalid mode: {s} (expected build, auto, plan, bypass)"))?;
    Ok(Some(mode))
}

pub fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if let Some(ref dir) = cli.dir {
        let dir_display = dir.display();
        std::env::set_current_dir(dir)
            .with_context(|| format!("failed to change directory to {dir_display}"))?;
    }

    let paths = Paths::discover();
    paths
        .ensure_dirs()
        .context("failed to create pacode directories")?;

    let config = pacode_config::load(&paths).context("failed to load config")?;

    let socket = cli
        .socket
        .clone()
        .or_else(|| config.daemon.socket.clone())
        .unwrap_or_else(|| paths.socket_path());

    let log_level =
        pacode_config::logging::level_from_env(std::env::var("PACODE_LOG").ok().as_deref())
            .unwrap_or_else(pacode_config::logging::default_level);

    match cli.command {
        Some(Command::Serve {
            detach,
            socket: serve_socket,
        }) => {
            let socket = serve_socket
                .or(cli.socket)
                .or_else(|| config.daemon.socket.clone())
                .unwrap_or_else(|| paths.socket_path());
            serve::run(detach, socket, paths, config, log_level)
        }
        Some(Command::Run {
            prompt,
            json,
            model,
            effort,
            mode,
            dir,
        }) => {
            if let Some(ref d) = dir {
                let d_display = d.display();
                std::env::set_current_dir(d)
                    .with_context(|| format!("failed to change directory to {d_display}"))?;
            }
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            log::info!(
                "pacode run starting (version={}, pid={}, log_level={log_level:?})",
                pacode_config::APP_VERSION,
                std::process::id()
            );
            let cwd = std::env::current_dir().context("failed to get current working directory")?;
            let model_route = parse_model_override(model.as_deref(), &config)?;
            let effort_level = parse_effort_override(effort.as_deref())?;
            let mode_val = parse_mode_override(mode.as_deref())?;
            let run_opts = run::RunOptions {
                prompt,
                json,
                model: model_route,
                effort: effort_level,
                mode: mode_val,
                cwd,
                socket: Some(socket),
                paths,
            };
            run::run(run_opts)
        }
        Some(Command::Sessions { action }) => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            log::info!(
                "pacode sessions cli starting (pid={}, log_level={log_level:?})",
                std::process::id()
            );
            sessions::run(action, Some(socket), paths)
        }
        Some(Command::Mcp { action }) => mcp_cmd::run(action, &paths),
        Some(Command::Plugins { action }) => plugins_cmd::run(action, &config),
        Some(Command::Import {
            from,
            mcp,
            skills,
            apply,
            force,
        }) => {
            let args = import_cmd::ImportArgs {
                from,
                mcp,
                skills,
                apply,
                force,
            };
            import_cmd::run(args, &paths)
        }
        Some(Command::Login { provider, list }) => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            log::info!(
                "pacode login cli starting (pid={}, log_level={log_level:?})",
                std::process::id()
            );
            login_cmd::run(provider, list, Some(socket), paths)
        }
        Some(Command::Daemon { action }) => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            log::info!(
                "pacode daemon cli starting (pid={}, log_level={log_level:?})",
                std::process::id()
            );
            daemon::run(action, socket)
        }
        #[cfg(feature = "acp")]
        Some(Command::Acp) => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            let mut client_opts = ClientOptions::new(paths.clone(), pacode_config::APP_VERSION);
            client_opts.socket = Some(socket);
            pacode_acp::run(client_opts, config).context("ACP agent failed")
        }
        None => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            log::info!(
                "pacode client starting (version={}, pid={}, log_level={log_level:?})",
                pacode_config::APP_VERSION,
                std::process::id()
            );
            let cwd = std::env::current_dir().context("failed to get current working directory")?;
            // Remembered choices (/model, /effort, /mode) win over config defaults;
            // explicit flags win over both.
            let prefs = pacode_config::load_prefs(&paths);
            let model_route = parse_model_override(cli.model.as_deref(), &config)?.or_else(|| {
                parse_model_override(prefs.model.as_deref(), &config)
                    .ok()
                    .flatten()
            });
            let effort_level = parse_effort_override(cli.effort.as_deref())?.or(prefs.effort);
            let mode_val = parse_mode_override(cli.mode.as_deref())?.or(prefs.mode);
            let attach = build_attach(
                cli.session,
                cwd.clone(),
                model_route,
                effort_level,
                mode_val,
            );

            let mut client_opts = ClientOptions::new(paths.clone(), pacode_config::APP_VERSION);
            client_opts.socket = Some(socket);

            // `[ui].color` overrides COLORTERM detection in pacode-render.
            if config.ui.color != "auto" && std::env::var_os("PACODE_COLOR").is_none() {
                // SAFETY: single-threaded at this point, before the runtime starts.
                unsafe { std::env::set_var("PACODE_COLOR", &config.ui.color) };
            }

            let tui_opts = pacode_tui::TuiOptions {
                client: client_opts,
                config,
                attach,
                initial_prompt: cli.prompt,
                cwd,
                app_version: pacode_config::APP_VERSION.to_string(),
            };

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to create tokio current_thread runtime")?;

            let session_id = rt
                .block_on(async { pacode_tui::run(tui_opts).await })
                .context("tui error")?;

            println!("Resume this session with:\n  pacode -s {session_id}");

            Ok(())
        }
    }
}
