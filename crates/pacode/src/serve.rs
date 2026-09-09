//! `pacode serve`: run the daemon in foreground or detached.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use pacode_config::Paths;
use pacode_daemon::DaemonOptions;
use pacode_types::Config;

pub fn run(
    detach: bool,
    socket: PathBuf,
    paths: Paths,
    config: Config,
    log_level: log::LevelFilter,
) -> anyhow::Result<()> {
    if detach {
        let exe = std::env::current_exe().context("failed to determine current executable")?;
        let pid = pacode_client::spawn_daemon(&exe, &socket, &paths.daemon_log(), &[])
            .context("failed to spawn detached daemon")?;
        println!("{pid}");
        return Ok(());
    }

    pacode_config::logging::init_file_logger(&paths.daemon_log(), log_level)
        .context("failed to initialize daemon logger")?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("failed to create tokio multi-thread runtime")?;

    rt.block_on(async move {
        let daemon_opts = DaemonOptions {
            paths: paths.clone(),
            config: Arc::new(config),
            socket,
            app_version: pacode_config::APP_VERSION.to_string(),
        };
        let core = pacode_daemon::build_core(&daemon_opts)
            .await
            .context("failed to build daemon core")?;
        pacode_daemon::run(daemon_opts, core)
            .await
            .context("daemon run error")?;
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}
