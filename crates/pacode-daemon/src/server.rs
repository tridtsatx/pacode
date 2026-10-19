//! Accept loop, idle timeout, signals.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pacode_core::Core;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::connection::{self, ServerControl};
use crate::wiring;
use crate::{DaemonError, DaemonOptions};

/// How often the daemon re-checks its idle timeout while nobody is attached.
/// With a client attached it waits for the disconnect instead of polling.
const IDLE_POLL_SECS: u64 = 30;
const MAX_CONNECTIONS: usize = 64;
/// How often a pending graceful shutdown re-checks whether the core went idle.
const GRACEFUL_IDLE_POLL_SECS: u64 = 2;

/// A session untouched this long (no running work, no subscriber) is dropped from memory
/// by [`Core::evict_idle_sessions`]; it reloads from the store on the next `Resume`.
const SESSION_MAX_IDLE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Serve until shutdown. Returns after cleanup.
pub async fn run(opts: DaemonOptions, core: Arc<Core>) -> Result<(), DaemonError> {
    let attach_store = wiring::shared_store(&opts.paths.db_file())?;
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

    // One-shot background warm-up of the model catalog
    let providers_for_warmup = core.providers();
    tokio::spawn(async move {
        providers_for_warmup.prefetch().await;
    });

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(DaemonError::Io)?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(DaemonError::Io)?;

    let control = ServerControl {
        shutdown: CancellationToken::new(),
        graceful_shutdown: CancellationToken::new(),
        connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        app_version: opts.app_version.clone(),
        pid,
        paths: opts.paths.clone(),
        disconnected: Arc::new(tokio::sync::Notify::new()),
        attach_store,
    };
    let admission = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connection_tasks = JoinSet::new();

    let mut idle_since: Option<std::time::Instant> = Some(std::time::Instant::now());
    // Whether freed heap pages were already returned to the OS since the last activity.
    let mut trimmed = false;
    let graceful_token = control.graceful_shutdown.clone();
    let mut graceful_request: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(graceful_token.cancelled());

    let core_for_watcher = Arc::clone(&core);
    let _watcher = pacode_config::ConfigWatcher::watch(
        opts.paths.config_file(),
        std::time::Duration::from_millis(150),
        move |result| {
            let core = Arc::clone(&core_for_watcher);
            tokio::spawn(async move {
                match result {
                    Ok(new_cfg) => {
                        let mut api_keys = std::collections::BTreeMap::new();
                        for (id, pcfg) in &new_cfg.providers {
                            api_keys.insert(id.clone(), pacode_config::resolve_api_key(pcfg));
                        }
                        if let Err(e) = core.reload_config(new_cfg.clone(), &api_keys) {
                            log::warn!("config reload failed: {e}");
                        }
                        let _ = core
                            .handle_global(&pacode_types::Request::ListMcpServers)
                            .await;
                        log::info!("reloaded configuration from disk");
                    }
                    Err(err) => {
                        log::error!("failed to parse configuration file: {err}");
                        core.broadcast_event(pacode_types::Event::Toast {
                            level: pacode_types::ToastLevel::Error,
                            title: "Configuration error".to_string(),
                            detail: Some(err.to_string()),
                        });
                    }
                }
            });
        },
    )
    .ok();

    loop {
        let conns = control
            .connections
            .load(std::sync::atomic::Ordering::Relaxed);
        let graceful = control.graceful_shutdown.is_cancelled();

        if graceful && core.is_idle() {
            log::info!("daemon shutting down: graceful shutdown requested and core is idle");
            break;
        }

        // The idle timer exists to shut a daemon down once nobody is attached.
        // While a client is attached there is nothing for it to decide, so it
        // waits for the disconnect instead of waking every couple of seconds —
        // which also stopped a full MCP status sweep from running on that tick.
        let idle_timer = async {
            if conns == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(IDLE_POLL_SECS)).await;
            } else {
                control.disconnected.notified().await;
            }
        };

        let graceful_idle_timer = async {
            if graceful {
                tokio::time::sleep(std::time::Duration::from_secs(GRACEFUL_IDLE_POLL_SECS)).await;
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
            _ = &mut graceful_request => {
                graceful_request = Box::pin(std::future::pending());
                if core.is_idle() {
                    log::info!("daemon shutting down: graceful shutdown requested and core is idle");
                    break;
                }
                log::info!("daemon graceful shutdown requested; waiting for the core to go idle");
            }
            _ = graceful_idle_timer => {
                if core.is_idle() {
                    log::info!("daemon shutting down: graceful shutdown requested and core is idle");
                    break;
                }
            }
            _ = idle_timer => {
                let current_conns = control.connections.load(std::sync::atomic::Ordering::Relaxed);
                // Housekeeping rides on this existing wake-up (a closed connection, or the
                // idle-shutdown poll when nobody is attached); it adds no timer of its own.
                let evicted = core.evict_idle_sessions(SESSION_MAX_IDLE);
                if evicted > 0 {
                    log::info!("evicted {evicted} idle session(s) from memory");
                }
                if current_conns == 0 && core.is_idle() {
                    // Once per busy period: a second trim with nothing new freed is wasted work.
                    if !trimmed {
                        release_freed_memory();
                        trimmed = true;
                    }
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
                    trimmed = false;
                }
            }
            task = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                if let Some(Err(error)) = task {
                    log::warn!("connection task failed: {error}");
                }
            }
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((mut stream, _addr)) => {
                        if control.graceful_shutdown.is_cancelled() {
                            log::warn!("daemon is shutting down gracefully; rejecting client");
                            let _ = stream.shutdown().await;
                            continue;
                        }
                        let permit = match Arc::clone(&admission).try_acquire_owned() {
                            Ok(permit) => permit,
                            Err(_) => {
                                log::warn!("daemon connection limit reached; rejecting client");
                                let _ = stream.shutdown().await;
                                continue;
                            }
                        };
                        idle_since = None;
                        trimmed = false;
                        let core_clone = Arc::clone(&core);
                        let control_clone = control.clone();
                        connection_tasks.spawn(async move {
                            let _permit = permit;
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

    control.shutdown.cancel();
    while let Some(result) = connection_tasks.join_next().await {
        if let Err(error) = result {
            log::warn!("connection task failed during shutdown: {error}");
        }
    }
    drop(control);
    core.shutdown().await;
    wiring::release_store(&opts.paths.db_file());
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

/// Hand freed heap pages back to the OS. glibc keeps the arenas of a busy period
/// (hundreds of agents streaming) resident long after the work is gone; this runs on the
/// existing idle poll only, never on a timer of its own, and only while nothing is
/// attached and the core is idle. A no-op on other allocators and platforms.
fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` is thread-safe and only releases unused pages.
    unsafe {
        libc::malloc_trim(0);
    }
}
