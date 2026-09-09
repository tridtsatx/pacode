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
    /// `$HOME/.pacode/...` and `/tmp/pacode-<uid>` when XDG variables are missing.
    pub fn discover() -> Paths {
        if let Some(home) = std::env::var_os("PACODE_HOME").filter(|s| !s.is_empty()) {
            return Paths::under(Path::new(&home));
        }

        let home_pacode = dirs::home_dir()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pacode");

        let config_file =
            if let Some(cfg) = std::env::var_os("PACODE_CONFIG").filter(|s| !s.is_empty()) {
                PathBuf::from(cfg)
            } else if let Some(dir) = dirs::config_dir() {
                dir.join("pacode").join("config.toml")
            } else {
                home_pacode.join("config.toml")
            };

        let data_dir = dirs::data_dir()
            .map(|d| d.join("pacode"))
            .unwrap_or_else(|| home_pacode.join("data"));

        let state_dir = dirs::state_dir()
            .or_else(dirs::data_dir)
            .map(|d| d.join("pacode"))
            .unwrap_or_else(|| home_pacode.join("state"));

        let cache_dir = dirs::cache_dir()
            .map(|d| d.join("pacode"))
            .unwrap_or_else(|| home_pacode.join("cache"));

        let runtime_dir =
            if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
                PathBuf::from(xdg).join("pacode")
            } else {
                #[cfg(unix)]
                let uid = unsafe { libc::getuid() };
                #[cfg(not(unix))]
                let uid = 1000;
                PathBuf::from(format!("/tmp/pacode-{uid}"))
            };

        Paths {
            config_file,
            data_dir,
            state_dir,
            cache_dir,
            runtime_dir,
        }
    }

    /// Everything under `root` (tests, `PACODE_HOME`).
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
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.state_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.runtime_dir)?;
        std::fs::create_dir_all(self.spool_dir())?;
        std::fs::create_dir_all(self.tool_output_dir())?;
        std::fs::create_dir_all(self.mcp_cache_dir())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.runtime_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        Ok(())
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("pacode.db")
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

    /// Pid file next to the socket, used by `pacode daemon status`.
    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir.join("daemon.pid")
    }
}
