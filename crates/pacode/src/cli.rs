//! clap definitions and command dispatch.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use pacode_client::ClientOptions;
use pacode_config::Paths;
use pacode_types::{Attach, Config, Effort, Mode, ModelRoute, SessionId};

#[path = "daemon.rs"]
mod daemon;
#[path = "run.rs"]
mod run;
#[path = "serve.rs"]
mod serve;
#[path = "sessions.rs"]
mod sessions;

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;

#[derive(Parser, Debug)]
#[command(name = "pacode", version, about = "background-first coding agent")]
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

pub fn build_attach(
    resume: Option<String>,
    cwd: PathBuf,
    model: Option<ModelRoute>,
    effort: Option<Effort>,
    mode: Option<Mode>,
) -> Attach {
    match resume {
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
        std::env::set_current_dir(dir)
            .with_context(|| format!("failed to change directory to {}", dir.display()))?;
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
            .unwrap_or(log::LevelFilter::Info);

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
                std::env::set_current_dir(d)
                    .with_context(|| format!("failed to change directory to {}", d.display()))?;
            }
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
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
            sessions::run(action, Some(socket), paths)
        }
        Some(Command::Daemon { action }) => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
            daemon::run(action, socket)
        }
        None => {
            pacode_config::logging::init_file_logger(&paths.client_log(), log_level)
                .context("failed to initialize client logger")?;
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
            let attach = build_attach(cli.resume, cwd.clone(), model_route, effort_level, mode_val);

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

            rt.block_on(async { pacode_tui::run(tui_opts).await })
                .context("tui error")?;

            Ok(())
        }
    }
}
