//! `codeapp sessions`: list and delete sessions.

use std::path::PathBuf;

use anyhow::{Context, bail};
use codeapp_client::{Client, ClientOptions};
use codeapp_config::Paths;
use codeapp_types::{Reply, Request};

use crate::cli::SessionsAction;

pub fn run(
    action: Option<SessionsAction>,
    socket: Option<PathBuf>,
    paths: Paths,
) -> anyhow::Result<()> {
    match action.unwrap_or(SessionsAction::List { limit: 20 }) {
        SessionsAction::List { limit } => list_sessions(limit, socket, paths),
        SessionsAction::Delete { id: _ } => {
            eprintln!("not supported yet");
            std::process::exit(1);
        }
    }
}

fn list_sessions(limit: u32, socket: Option<PathBuf>, paths: Paths) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        let mut client_opts = ClientOptions::new(paths, codeapp_config::APP_VERSION);
        client_opts.socket = socket;
        client_opts.spawn_daemon = true;

        let (client, _events_rx) = Client::connect(client_opts)
            .await
            .context("failed to connect to daemon")?;

        let reply = client
            .request(Request::ListSessions { limit })
            .await
            .context("failed to list sessions")?;

        let sessions = match reply {
            Reply::Sessions(sessions) => sessions,
            Reply::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected reply: {other:?}"),
        };

        println!("{:<20}  {:<10}  {:<28}  title", "id", "updated", "cwd");
        for s in sessions {
            let updated = format_relative_time(s.updated_at_ms);
            println!(
                "{:<20}  {:<10}  {:<28}  {}",
                s.id,
                updated,
                s.cwd.display(),
                s.title()
            );
        }

        Ok(())
    })
}

fn format_relative_time(ts_ms: u64) -> String {
    let now = codeapp_types::now_ms();
    let diff_ms = now.saturating_sub(ts_ms);
    let secs = diff_ms / 1000;
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
