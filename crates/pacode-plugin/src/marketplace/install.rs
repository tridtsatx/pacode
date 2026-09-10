//! Installing a plugin out of a marketplace repository.
//!
//! The repository arrives as one gzipped tar, and only the plugin's own
//! directory is unpacked. Every path in that archive is treated as hostile: an
//! entry that would land outside the destination is refused, not sanitised.
//! Installation stages into a temporary directory and is moved into place at the
//! end, so an interrupted install never leaves a half-plugin behind.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::PluginEntry;
use super::source::MarketplaceSource;

/// Most files and most bytes one plugin may unpack to.
pub const MAX_FILES: usize = 5000;
pub const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("the archive entry {0:?} would be written outside the plugin directory")]
    PathEscape(String),
    #[error("the archive holds more than {MAX_FILES} files")]
    TooManyFiles,
    #[error("the archive unpacks to more than {MAX_UNPACKED_BYTES} bytes")]
    TooLarge,
    #[error("no directory {0:?} in the archive")]
    NotInArchive(String),
    #[error("{0} cannot be installed from a plain URL; use owner/repo")]
    NeedsRepository(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot read the installed-plugin record: {0}")]
    Record(#[from] serde_json::Error),
}

/// What was installed, where it came from, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub name: String,
    /// The marketplace it came from, in its `owner/repo[@ref]` form.
    pub source: String,
    /// The ref the files were taken from, when the source pinned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default)]
    pub version: String,
    pub installed_at_ms: u64,
    /// Files written, relative to the plugin directory, so uninstall removes
    /// exactly what was installed and nothing a user put there afterwards.
    #[serde(default)]
    pub files: Vec<String>,
}

/// Name of the record kept beside each installed plugin.
pub const RECORD_FILE: &str = ".pacode-install.json";

/// Unpack `entry`'s directory out of a repository tarball into `dest`.
///
/// GitHub's tarball wraps everything in a single top-level directory, which is
/// stripped; `entry.source` is then the path inside the repository.
pub fn unpack_plugin(
    archive: &[u8],
    entry: &PluginEntry,
    dest: &Path,
) -> Result<Vec<String>, InstallError> {
    unpack_subdir(archive, &entry.source, &entry.name, dest)
}

/// Unpack `subpath` out of a repository tarball. An empty `subpath` takes the
/// whole repository, which is what a plugin that *is* a repository needs.
pub fn unpack_subdir(
    archive: &[u8],
    subpath: &str,
    plugin_name: &str,
    dest: &Path,
) -> Result<Vec<String>, InstallError> {
    let wanted = plugin_subpath(subpath);
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);

    std::fs::create_dir_all(dest)?;

    let mut written = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut found = false;

    for item in tar.entries()? {
        let mut item = item?;
        let path = item.path()?.into_owned();

        // Strip the archive's single wrapping directory.
        let mut parts = path.components();
        parts.next();
        let inner: PathBuf = parts.collect();

        let Ok(relative) = inner.strip_prefix(&wanted) else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            found = true;
            continue;
        }
        found = true;

        let safe = safe_relative(relative)?;
        if written.len() >= MAX_FILES {
            return Err(InstallError::TooManyFiles);
        }
        total_bytes = total_bytes.saturating_add(item.size());
        if total_bytes > MAX_UNPACKED_BYTES {
            return Err(InstallError::TooLarge);
        }

        let target = dest.join(&safe);
        // Belt and braces: the joined path must still be inside `dest` even if
        // the entry got past the component check.
        if !target.starts_with(dest) {
            return Err(InstallError::PathEscape(safe.display().to_string()));
        }

        if item.header().entry_type().is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        // A symlink could point anywhere once the plugin directory is read, so
        // links are dropped rather than followed.
        if item.header().entry_type().is_symlink() {
            log::warn!("skipping symlink {safe:?} in plugin {plugin_name}");
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        item.read_to_end(&mut buf)?;
        std::fs::write(&target, &buf)?;
        written.push(safe.to_string_lossy().to_string());
    }

    if !found {
        return Err(InstallError::NotInArchive(wanted.display().to_string()));
    }
    Ok(written)
}

/// Where a plugin's files come from.
///
/// An index is a list of pointers, so an entry may name a directory inside the
/// marketplace repository or another repository entirely — the second form is
/// what lets a marketplace list a plugin maintained elsewhere without vendoring
/// a copy of it that goes stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginSource {
    /// A directory inside the marketplace repository.
    InRepo(String),
    /// Another repository, optionally with a directory inside it after `#`.
    Repository {
        source: super::MarketplaceSource,
        subdir: Option<String>,
    },
}

impl PluginSource {
    /// Read an entry's `source` field.
    ///
    /// `./skills-pack`, `/foo` and a bare `foo` are directories in this
    /// repository; `owner/repo`, `owner/repo@ref` and `owner/repo@ref#sub/dir`
    /// are elsewhere, and so is an https URL to a repository.
    ///
    /// A bare `a/b` reads as `owner/repo`, which is why a nested directory in
    /// this repository is written with the leading `./` upstream uses.
    pub fn parse(source: &str) -> Self {
        let trimmed = source.trim();
        let looks_local = trimmed.starts_with("./")
            || trimmed.starts_with("../")
            || trimmed.starts_with('/')
            || !trimmed.contains('/');
        if looks_local {
            return Self::InRepo(trimmed.to_string());
        }

        let (spec, subdir) = match trimmed.split_once('#') {
            Some((spec, sub)) if !sub.is_empty() => (spec, Some(sub.to_string())),
            Some((spec, _)) => (spec, None),
            None => (trimmed, None),
        };
        match super::MarketplaceSource::parse(spec) {
            Ok(source) => Self::Repository { source, subdir },
            // Anything that is not a repository spec is treated as a path, which
            // the traversal guards then have to accept before it is used.
            Err(_) => Self::InRepo(trimmed.to_string()),
        }
    }
}

/// `./foo`, `foo/bar` and `/foo` all mean the same directory inside the archive.
pub(crate) fn plugin_subpath(source: &str) -> PathBuf {
    let trimmed = source
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/');
    PathBuf::from(trimmed)
}

/// A relative path with no `..`, no root and no prefix, or an error.
fn safe_relative(path: &Path) -> Result<PathBuf, InstallError> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::PathEscape(path.display().to_string()));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(InstallError::PathEscape(path.display().to_string()));
    }
    Ok(out)
}

/// Move a staged directory into its final place, replacing whatever was there.
pub fn commit_staged(staged: &Path, final_dir: &Path) -> Result<(), InstallError> {
    if let Some(parent) = final_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if final_dir.exists() {
        std::fs::remove_dir_all(final_dir)?;
    }
    match std::fs::rename(staged, final_dir) {
        Ok(()) => Ok(()),
        // A staging directory on another filesystem cannot be renamed across;
        // copy it over instead of failing the install.
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_dir(staged, final_dir)?;
            std::fs::remove_dir_all(staged)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Read the record written at install time, when there is one.
pub fn read_record(plugin_dir: &Path) -> Option<InstalledPlugin> {
    let bytes = std::fs::read(plugin_dir.join(RECORD_FILE)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(record) => Some(record),
        Err(e) => {
            log::warn!("unreadable install record in {plugin_dir:?}: {e}");
            None
        }
    }
}

pub fn write_record(plugin_dir: &Path, record: &InstalledPlugin) -> Result<(), InstallError> {
    let bytes = serde_json::to_vec_pretty(record)?;
    std::fs::write(plugin_dir.join(RECORD_FILE), bytes)?;
    Ok(())
}

/// The source spec a plugin was installed from, as a parsed source.
pub fn record_source(record: &InstalledPlugin) -> Option<MarketplaceSource> {
    MarketplaceSource::parse(&record.source).ok()
}
