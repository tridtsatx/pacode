//! Schema cache on disk and in memory for MCP servers.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use pacode_types::McpServerConfig;
use serde::{Deserialize, Serialize};

use crate::{McpPrompt, McpResource, McpToolInfo};

pub const CACHE_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskCacheEntry {
    #[serde(default)]
    pub version: u32,
    pub fingerprint: String,
    pub tools: Vec<McpToolInfo>,
    #[serde(default)]
    pub resources: Vec<McpResource>,
    #[serde(default)]
    pub prompts: Vec<McpPrompt>,
}

#[derive(Clone, Debug, Default)]
pub struct ServerSchemaCache {
    pub tools: Vec<McpToolInfo>,
    pub resources: Vec<McpResource>,
    pub prompts: Vec<McpPrompt>,
}

/// Fingerprint of a server's launch configuration.
pub fn fingerprint(cfg: &McpServerConfig) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "v2".hash(&mut hasher);
    cfg.command.hash(&mut hasher);
    for arg in &cfg.args {
        arg.hash(&mut hasher);
        0u8.hash(&mut hasher);
    }
    for (k, v) in &cfg.env {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
        0u8.hash(&mut hasher);
    }
    cfg.url.hash(&mut hasher);
    for (k, v) in &cfg.headers {
        k.hash(&mut hasher);
        v.hash(&mut hasher);
    }
    cfg.enabled.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub fn load_disk_cache(
    cache_dir: &Path,
    server: &str,
    cfg: &McpServerConfig,
    mem: &mut HashMap<String, ServerSchemaCache>,
) -> Option<ServerSchemaCache> {
    let path = cache_dir.join(format!("{server}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    let entry: DiskCacheEntry = serde_json::from_str(&content).ok()?;
    if entry.version == CACHE_VERSION && entry.fingerprint == fingerprint(cfg) {
        let cache = ServerSchemaCache {
            tools: entry.tools,
            resources: entry.resources,
            prompts: entry.prompts,
        };
        mem.insert(server.to_string(), cache.clone());
        Some(cache)
    } else {
        None
    }
}

pub fn save_disk_cache(
    cache_dir: &Path,
    server: &str,
    cfg: &McpServerConfig,
    tools: &[McpToolInfo],
    resources: &[McpResource],
    prompts: &[McpPrompt],
) {
    if let Err(err) = std::fs::create_dir_all(cache_dir) {
        log::warn!(
            "Failed to create schema cache dir {}: {err}",
            cache_dir.display()
        );
        return;
    }
    let path = cache_dir.join(format!("{server}.json"));
    let entry = DiskCacheEntry {
        version: CACHE_VERSION,
        fingerprint: fingerprint(cfg),
        tools: tools.to_vec(),
        resources: resources.to_vec(),
        prompts: prompts.to_vec(),
    };
    match serde_json::to_string_pretty(&entry) {
        Ok(json) => {
            if let Err(err) = std::fs::write(&path, json) {
                log::warn!("Failed to write schema cache to {}: {err}", path.display());
            }
        }
        Err(err) => {
            log::warn!("Failed to serialize schema cache: {err}");
        }
    }
}
