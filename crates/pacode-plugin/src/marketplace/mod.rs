//! The plugin marketplace: a public repository listing plugins, in the Claude
//! Code layout so plugins written for it work here unchanged.
//!
//! What this module owns: fetching and caching an index, installing a plugin's
//! directory out of the repository, and answering the queries a picker needs.
//! What it deliberately does not own: running the plugin. That is `runtime`.

pub mod cache;
pub mod install;
pub mod manifest;
pub mod source;

#[cfg(test)]
#[path = "marketplace_tests.rs"]
mod marketplace_tests;

use std::path::PathBuf;
use std::sync::Arc;

pub use cache::{CachedIndex, IndexCache};
pub use install::{InstallError, InstalledPlugin};
pub use manifest::{Component, MarketplaceIndex, PluginEntry, PluginManifest};
pub use source::{FetchError, HttpFetcher, MarketplaceFetcher, MarketplaceSource, SourceError};

#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("the marketplace index is not readable json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("no plugin named {0:?} in this marketplace")]
    UnknownPlugin(String),
    #[error("the plugin name {0:?} is not a plain directory name")]
    UnsafeName(String),
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One plugin as a picker shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginListing {
    pub entry: PluginEntry,
    /// The marketplace it came from, in display form.
    pub marketplace: String,
    /// The installed record, when this plugin is installed.
    pub installed: Option<InstalledPlugin>,
}

impl PluginListing {
    /// Whether the marketplace lists a different version from the installed one.
    pub fn update_available(&self) -> bool {
        match &self.installed {
            Some(installed) => {
                !self.entry.version.is_empty() && installed.version != self.entry.version
            }
            None => false,
        }
    }
}

/// Reads marketplaces and installs from them.
pub struct Marketplace {
    fetcher: Arc<dyn MarketplaceFetcher>,
    cache: IndexCache,
    /// Where installed plugins live; one directory per plugin.
    plugins_dir: PathBuf,
}

impl Marketplace {
    pub fn new(
        fetcher: Arc<dyn MarketplaceFetcher>,
        cache_dir: PathBuf,
        plugins_dir: PathBuf,
        ttl_secs: u64,
    ) -> Self {
        Self {
            fetcher,
            cache: IndexCache::new(cache_dir, ttl_secs),
            plugins_dir,
        }
    }

    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.plugins_dir
    }

    /// The index for `source`: cached when fresh, fetched when stale, and served
    /// stale (marked as such) when the fetch fails.
    pub async fn index(
        &self,
        source: &MarketplaceSource,
        now_ms: u64,
    ) -> Result<CachedIndex, MarketplaceError> {
        let key = source.cache_key();
        let cached = self.cache.get(&key, now_ms);
        if let Some(hit) = &cached
            && !hit.stale
        {
            return Ok(hit.clone());
        }

        match self
            .fetcher
            .get(&source.index_url(), source::INDEX_MAX_BYTES)
            .await
        {
            Ok(bytes) => {
                let index = MarketplaceIndex::parse(&bytes)?;
                self.cache.put(&key, &index, now_ms);
                Ok(CachedIndex {
                    index,
                    fetched_at_ms: now_ms,
                    stale: false,
                })
            }
            Err(e) => match cached {
                // A stale copy is worth more than an error screen.
                Some(hit) => {
                    log::warn!(
                        "serving a stale marketplace index for {}: {e}",
                        source.display()
                    );
                    Ok(hit)
                }
                None => Err(e.into()),
            },
        }
    }

    /// Every plugin in `source`, with its installed state, filtered by `query`.
    pub async fn list(
        &self,
        source: &MarketplaceSource,
        query: &str,
        now_ms: u64,
    ) -> Result<Vec<PluginListing>, MarketplaceError> {
        let cached = self.index(source, now_ms).await?;
        let installed = self.installed();
        let needle = query.trim().to_lowercase();

        Ok(cached
            .index
            .plugins
            .into_iter()
            .filter(|entry| matches(entry, &needle))
            .map(|entry| {
                let installed = installed.iter().find(|i| i.name == entry.name).cloned();
                PluginListing {
                    entry,
                    marketplace: source.display(),
                    installed,
                }
            })
            .collect())
    }

    /// Plugins installed on this machine, from their records.
    pub fn installed(&self) -> Vec<InstalledPlugin> {
        let Ok(entries) = std::fs::read_dir(&self.plugins_dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| install::read_record(&e.path()))
            .collect()
    }

    /// Install one plugin from `source`, replacing any previous copy.
    pub async fn install(
        &self,
        source: &MarketplaceSource,
        plugin: &str,
        now_ms: u64,
    ) -> Result<InstalledPlugin, MarketplaceError> {
        let cached = self.index(source, now_ms).await?;
        let entry = cached
            .index
            .find(plugin)
            .cloned()
            .ok_or_else(|| MarketplaceError::UnknownPlugin(plugin.to_string()))?;

        let archive_url = source
            .archive_url()
            .ok_or_else(|| InstallError::NeedsRepository(source.display()))?;
        let archive = self
            .fetcher
            .get(&archive_url, source::ARCHIVE_MAX_BYTES)
            .await?;

        // The name reached us from a fetched index; nothing is joined onto a path
        // until it is proven to be one plain directory name.
        if !manifest::is_valid_plugin_name(&entry.name) {
            return Err(MarketplaceError::UnsafeName(entry.name.clone()));
        }

        // Stage inside the plugins directory so the final move is a rename on the
        // same filesystem, and so a failure leaves nothing where plugins are read.
        std::fs::create_dir_all(&self.plugins_dir)?;
        let staged = self.plugins_dir.join(format!(".staging-{}", entry.name));
        // Belt and braces: both paths must sit directly in the plugins directory
        // before anything is removed or renamed.
        if staged.parent() != Some(self.plugins_dir.as_path()) {
            return Err(MarketplaceError::UnsafeName(entry.name.clone()));
        }
        if staged.exists() {
            std::fs::remove_dir_all(&staged)?;
        }
        let unpack = install::unpack_plugin(&archive, &entry, &staged);
        let files = match unpack {
            Ok(files) => files,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staged);
                return Err(e.into());
            }
        };

        let reference = match source {
            MarketplaceSource::GitHub { reference, .. } => reference.clone(),
            MarketplaceSource::Url(_) => None,
        };
        let record = InstalledPlugin {
            name: entry.name.clone(),
            source: source.display(),
            reference,
            version: entry.version.clone(),
            installed_at_ms: now_ms,
            files,
        };
        install::write_record(&staged, &record)?;

        let final_dir = self.plugins_dir.join(&entry.name);
        if final_dir.parent() != Some(self.plugins_dir.as_path()) {
            return Err(MarketplaceError::UnsafeName(entry.name.clone()));
        }
        install::commit_staged(&staged, &final_dir)?;
        Ok(record)
    }

    /// Remove exactly what an install wrote, and the record with it.
    pub fn uninstall(&self, plugin: &str) -> Result<(), MarketplaceError> {
        if !manifest::is_valid_plugin_name(plugin) {
            return Err(MarketplaceError::UnsafeName(plugin.to_string()));
        }
        let dir = self.plugins_dir.join(plugin);
        let Some(record) = install::read_record(&dir) else {
            // Nothing pacode installed: refuse rather than delete a directory a
            // person put there by hand.
            return Err(MarketplaceError::UnknownPlugin(plugin.to_string()));
        };
        for file in &record.files {
            let path = dir.join(file);
            if path.starts_with(&dir) {
                let _ = std::fs::remove_file(path);
            }
        }
        let _ = std::fs::remove_file(dir.join(install::RECORD_FILE));
        // Directories left empty by those removals go too; anything else stays.
        remove_empty_dirs(&dir);
        Ok(())
    }

    /// Components of an installed plugin that pacode will not run.
    pub fn unsupported_components(&self, plugin: &str) -> Vec<Component> {
        if !manifest::is_valid_plugin_name(plugin) {
            return Vec::new();
        }
        let manifest_path = self
            .plugins_dir
            .join(plugin)
            .join(".claude-plugin")
            .join("plugin.json");
        let Ok(bytes) = std::fs::read(manifest_path) else {
            return Vec::new();
        };
        match PluginManifest::parse(&bytes) {
            Ok(manifest) => manifest::unsupported_components(&manifest),
            Err(e) => {
                log::warn!("unreadable plugin.json for {plugin}: {e}");
                Vec::new()
            }
        }
    }
}

fn matches(entry: &PluginEntry, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    entry.name.to_lowercase().contains(needle)
        || entry.description.to_lowercase().contains(needle)
        || entry.category.to_lowercase().contains(needle)
        || entry
            .keywords
            .iter()
            .any(|k| k.to_lowercase().contains(needle))
}

fn remove_empty_dirs(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            remove_empty_dirs(&entry.path());
        }
    }
    let _ = std::fs::remove_dir(dir);
}
