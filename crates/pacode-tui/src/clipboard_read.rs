//! Reading system clipboard images and managing temporary pasted image files.

use std::path::{Path, PathBuf};

use pacode_types::time::now_ms;

#[cfg(test)]
#[path = "clipboard_read_tests.rs"]
pub(crate) mod clipboard_read_tests;

pub const IMAGE_PASTE_MAX_BYTES: usize = 20 * 1024 * 1024; // 20 MiB
pub const PASTED_IMAGES_MAX_COUNT: usize = 16;
pub const PASTED_IMAGES_MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024; // 50 MiB

/// Command execution abstraction for testing clipboard reading without display or external tools.
pub trait CommandRunner: Send + Sync {
    /// Returns true if `binary` is available in PATH.
    fn which(&self, binary: &str) -> bool;

    /// Runs `binary` with `args` and returns the command stdout on success.
    fn run(&self, binary: &str, args: &[&str]) -> Result<Vec<u8>, String>;
}

/// Real system command runner that executes subprocesses.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn which(&self, binary: &str) -> bool {
        if let Some(path_val) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_val) {
                let candidate = dir.join(binary);
                if candidate.is_file() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(meta) = candidate.metadata()
                            && meta.permissions().mode() & 0o111 != 0
                        {
                            return true;
                        }
                    }
                    #[cfg(not(unix))]
                    return true;
                }
            }
        }
        false
    }

    fn run(&self, binary: &str, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = std::process::Command::new(binary)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;

        if output.status.success() {
            Ok(output.stdout)
        } else {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("command exited with status {}", output.status)
            } else {
                err
            })
        }
    }
}

/// Target operating system for selecting the appropriate clipboard helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardPlatform {
    Linux,
    MacOs,
    Other,
}

impl ClipboardPlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Helper program used to read PNG image data from the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardHelper {
    WlPaste,
    Xclip,
    Pngpaste,
}

/// Detects the available clipboard image helper program.
///
/// On Linux, tries Wayland (`wl-paste`) first, then X11 (`xclip`).
/// On macOS, checks for `pngpaste`.
pub fn detect_clipboard_helper(
    runner: &dyn CommandRunner,
    platform: ClipboardPlatform,
) -> Result<ClipboardHelper, String> {
    match platform {
        ClipboardPlatform::Linux => {
            if runner.which("wl-paste") {
                Ok(ClipboardHelper::WlPaste)
            } else if runner.which("xclip") {
                Ok(ClipboardHelper::Xclip)
            } else {
                Err("No clipboard helper found. Install wl-paste (wl-clipboard) for Wayland or xclip for X11 to paste images.".to_string())
            }
        }
        ClipboardPlatform::MacOs => {
            if runner.which("pngpaste") {
                Ok(ClipboardHelper::Pngpaste)
            } else {
                Err("No clipboard helper found. Install pngpaste (brew install pngpaste) to paste images.".to_string())
            }
        }
        ClipboardPlatform::Other => {
            Err("Pasting images is not supported on this platform.".to_string())
        }
    }
}

/// Outcome of reading image data from the system clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum ClipboardImageResult {
    /// Image data read successfully.
    Image(Vec<u8>),
    /// Helper ran, but clipboard does not contain an image.
    NoImage,
    /// Helper program is not installed.
    HelperMissing(String),
}

/// Reads image data from the clipboard using the specified runner and platform.
pub fn read_image_data(
    runner: &dyn CommandRunner,
    platform: ClipboardPlatform,
) -> ClipboardImageResult {
    let helper = match detect_clipboard_helper(runner, platform) {
        Ok(h) => h,
        Err(msg) => return ClipboardImageResult::HelperMissing(msg),
    };

    let result = match helper {
        ClipboardHelper::WlPaste => {
            runner.run("wl-paste", &["--type", "image/png", "--no-newline"])
        }
        ClipboardHelper::Xclip => runner.run(
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
        ClipboardHelper::Pngpaste => runner.run("pngpaste", &["-"]),
    };

    match result {
        Ok(bytes) => {
            if bytes.is_empty() {
                ClipboardImageResult::NoImage
            } else {
                ClipboardImageResult::Image(bytes)
            }
        }
        Err(_) => {
            // On Linux, if wl-paste was preferred but failed (e.g. not a Wayland session),
            // fall back to xclip if present.
            if helper == ClipboardHelper::WlPaste
                && runner.which("xclip")
                && let Ok(bytes) = runner.run(
                    "xclip",
                    &["-selection", "clipboard", "-t", "image/png", "-o"],
                )
                && !bytes.is_empty()
            {
                return ClipboardImageResult::Image(bytes);
            }
            ClipboardImageResult::NoImage
        }
    }
}

/// Metadata record for a temporary image file created from a paste.
#[derive(Debug, Clone)]
pub struct PastedImageRecord {
    pub path: PathBuf,
    pub bytes_len: usize,
    pub submitted: bool,
}

/// Owns temporary image files created during clipboard paste operations.
///
/// - Storage location: `$TMPDIR/pacode-pastes-<pid>` (or a configured directory).
/// - Size cap: 20 MiB per image file, 16 files maximum, 50 MiB total disk cap.
/// - Lifecycle and ownership: `PastedImages` owns the files. An image pasted and then
///   removed from the prompt is deleted immediately by `cleanup_unreferenced`.
///   Submitted images remain until the process terminates, at which point `Drop`
///   cleans up all remaining temporary files and the temporary directory.
#[derive(Debug)]
pub struct PastedImages {
    temp_dir: PathBuf,
    records: Vec<PastedImageRecord>,
    counter: usize,
}

impl Default for PastedImages {
    fn default() -> Self {
        Self::new()
    }
}

impl PastedImages {
    /// Creates a new `PastedImages` manager using the standard temporary directory.
    pub fn new() -> Self {
        let temp_dir = std::env::temp_dir().join(format!("pacode-pastes-{}", std::process::id()));
        Self {
            temp_dir,
            records: Vec::new(),
            counter: 0,
        }
    }

    /// Creates a new `PastedImages` manager in a specified directory (useful for testing).
    pub fn new_in(temp_dir: PathBuf) -> Self {
        Self {
            temp_dir,
            records: Vec::new(),
            counter: 0,
        }
    }

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    pub fn records(&self) -> &[PastedImageRecord] {
        &self.records
    }

    /// Writes image bytes to a new temporary `.png` file, enforcing caps.
    pub fn create_image_file(&mut self, bytes: &[u8]) -> Result<PathBuf, std::io::Error> {
        if bytes.len() > IMAGE_PASTE_MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("image size {} bytes exceeds 20 MiB limit", bytes.len()),
            ));
        }

        std::fs::create_dir_all(&self.temp_dir)?;

        // Enforce maximum file count and total byte cap by pruning oldest unsubmitted files.
        while self.records.len() >= PASTED_IMAGES_MAX_COUNT
            || self.total_bytes() + bytes.len() > PASTED_IMAGES_MAX_TOTAL_BYTES
        {
            if let Some(pos) = self.records.iter().position(|r| !r.submitted) {
                let removed = self.records.remove(pos);
                let _ = std::fs::remove_file(&removed.path);
            } else {
                break;
            }
        }

        self.counter += 1;
        let file_name = format!("paste_{}_{}.png", now_ms(), self.counter);
        let file_path = self.temp_dir.join(file_name);

        std::fs::write(&file_path, bytes)?;

        self.records.push(PastedImageRecord {
            path: file_path.clone(),
            bytes_len: bytes.len(),
            submitted: false,
        });

        Ok(file_path)
    }

    /// Deletes any unsubmitted pasted image files that are no longer referenced in `prompt_text`.
    pub fn cleanup_unreferenced(&mut self, prompt_text: &str) {
        self.records.retain(|record| {
            if record.submitted {
                return true;
            }
            let path_str = record.path.to_string_lossy();
            if prompt_text.contains(path_str.as_ref()) {
                true
            } else {
                let _ = std::fs::remove_file(&record.path);
                false
            }
        });
    }

    /// Marks images referenced in `prompt_text` as submitted so they are preserved
    /// after the prompt is cleared for submission to the daemon.
    pub fn mark_submitted(&mut self, prompt_text: &str) {
        for record in &mut self.records {
            let path_str = record.path.to_string_lossy();
            if prompt_text.contains(path_str.as_ref()) {
                record.submitted = true;
            }
        }
    }

    fn total_bytes(&self) -> usize {
        self.records.iter().map(|r| r.bytes_len).sum()
    }
}

impl Drop for PastedImages {
    fn drop(&mut self) {
        for record in &self.records {
            let _ = std::fs::remove_file(&record.path);
        }
        let _ = std::fs::remove_dir(&self.temp_dir);
    }
}
