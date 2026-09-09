//! Accept loop, idle timeout, signals.

use std::sync::Arc;

use pacode_core::Core;

use crate::connection::{self, ServerControl};
use crate::{DaemonError, DaemonOptions};

/// Serve until shutdown. Returns after cleanup.
pub async fn run(opts: DaemonOptions, core: Arc<Core>) -> Result<(), DaemonError> {
    let listener = bind_socket(&opts.socket).await?;

    let pid = std::process::id();
    let pid_file = opts.paths.pid_file();
    if let Some(parent) = pid_file.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(DaemonError::Io)?;
    }
    std::fs::write(&pid_file, format!("{pid}\n")).map_err(DaemonError::Io)?;

    log::info!("daemon listening on {}", opts.socket.display());

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(DaemonError::Io)?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(DaemonError::Io)?;

    let control = ServerControl {
        shutdown: tokio_util::sync::CancellationToken::new(),
        connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        app_version: opts.app_version.clone(),
        pid,
        paths: opts.paths.clone(),
        shutdown_when_idle: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let mut idle_since: Option<std::time::Instant> = Some(std::time::Instant::now());

    loop {
        let conns = control
            .connections
            .load(std::sync::atomic::Ordering::Relaxed);
        let shutdown_idle = control
            .shutdown_when_idle
            .load(std::sync::atomic::Ordering::Relaxed);

        if shutdown_idle && conns == 0 && core.is_idle() {
            log::info!("daemon shutting down: shutdown_when_idle is set and core is idle");
            break;
        }

        let idle_timer = async {
            if conns == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        };

        let shutdown_idle_timer = async {
            if shutdown_idle {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            _ = sigterm.recv() => {
                log::info!("daemon received SIGTERM");
                break;
            }
            _ = sigint.recv() => {
                log::info!("daemon received SIGINT");
                break;
            }
            _ = control.shutdown.cancelled() => {
                log::info!("daemon shutdown requested via cancellation token");
                break;
            }
            _ = shutdown_idle_timer => {
                let current_conns = control.connections.load(std::sync::atomic::Ordering::Relaxed);
                if current_conns == 0 && core.is_idle() {
                    log::info!("daemon shutting down: shutdown_when_idle timer fired and core is idle");
                    break;
                }
            }
            _ = idle_timer => {
                let current_conns = control.connections.load(std::sync::atomic::Ordering::Relaxed);
                if current_conns == 0 && core.is_idle() {
                    let idle_start = idle_since.get_or_insert_with(std::time::Instant::now);
                    if opts.config.daemon.idle_timeout_secs > 0
                        && idle_start.elapsed() >= std::time::Duration::from_secs(opts.config.daemon.idle_timeout_secs)
                    {
                        log::info!(
                            "daemon idle timeout of {}s reached; shutting down",
                            opts.config.daemon.idle_timeout_secs
                        );
                        break;
                    }
                } else {
                    idle_since = None;
                }
            }
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((stream, _addr)) => {
                        idle_since = None;
                        let core_clone = Arc::clone(&core);
                        let control_clone = control.clone();
                        tokio::spawn(async move {
                            connection::serve_connection(stream, core_clone, control_clone).await;
                        });
                    }
                    Err(err) => {
                        log::warn!("daemon listener accept error: {err}");
                    }
                }
            }
        }
    }

    core.shutdown().await;
    let _ = std::fs::remove_file(&opts.socket);
    let _ = std::fs::remove_file(&pid_file);

    Ok(())
}

/// Bind the listener: if the socket file exists and a connect attempt succeeds →
/// `SocketBusy`; if it exists but refuses → remove and bind; set mode 0600.
pub async fn bind_socket(path: &std::path::Path) -> Result<tokio::net::UnixListener, DaemonError> {
    if std::fs::symlink_metadata(path).is_ok() || path.exists() {
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => {
                return Err(DaemonError::SocketBusy {
                    path: path.to_path_buf(),
                });
            }
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(DaemonError::Io)?;
    }

    let listener = tokio::net::UnixListener::bind(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(DaemonError::Io)?;
    }

    Ok(listener)
}
