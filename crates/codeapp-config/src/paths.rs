//! Filesystem layout.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    /// Resolve from the environment (see crate docs). Never fails: falls back to
    /// `$HOME/.codeapp/...` and `/tmp/codeapp-<uid>` when XDG variables are missing.
    pub fn discover() -> Paths {
        todo!("Paths::discover")
    }

    /// Everything under `root` (tests, `CODEAPP_HOME`).
    pub fn under(root: &Path) -> Paths {
        Paths {
            config_file: root.join("config.toml"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("run"),
        }
    }

    /// Create data/state/cache/runtime dirs (runtime dir mode 0700).
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        todo!("Paths::ensure_dirs")
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("codeapp.db")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.state_dir.join("daemon.log")
    }

    pub fn client_log(&self) -> PathBuf {
        self.state_dir.join("client.log")
    }

    pub fn spool_dir(&self) -> PathBuf {
        self.state_dir.join("tasks")
    }

    pub fn tool_output_dir(&self) -> PathBuf {
        self.state_dir.join("tool-output")
    }

    pub fn mcp_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("mcp")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("daemon.sock")
    }

    /// Pid file next to the socket, used by `codeapp daemon status`.
    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir.join("daemon.pid")
    }
}
