//! `pacode daemon`: status and stop.

use std::path::PathBuf;

use anyhow::Context;

use crate::cli::DaemonAction;

pub fn run(action: DaemonAction, socket: PathBuf) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        match action {
            DaemonAction::Status => {
                match pacode_client::daemon_status(&socket, pacode_config::APP_VERSION).await {
                    Some(status) => {
                        println!("pid: {}, version: {}", status.pid, status.version);
                        Ok(())
                    }
                    None => {
                        eprintln!("not running");
                        std::process::exit(1);
                    }
                }
            }
            DaemonAction::Stop { force } => {
                pacode_client::stop_daemon(&socket, pacode_config::APP_VERSION, force)
                    .await
                    .context("failed to stop daemon")?;
                println!("daemon stopped");
                Ok(())
            }
        }
    })
}
