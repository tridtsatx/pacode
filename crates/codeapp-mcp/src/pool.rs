//! Server pool with lazy start and schema cache.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codeapp_types::McpServerConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CacheDir, McpCallResult, McpClient, McpError, McpToolInfo};

#[derive(Debug, Serialize, Deserialize)]
struct DiskCacheEntry {
    fingerprint: String,
    tools: Vec<McpToolInfo>,
}

/// Fingerprint of a server's launch configuration.
pub fn fingerprint(cfg: &McpServerConfig) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cfg.command.hash(&mut hasher);
    for arg in &cfg.args {
        arg.hash(&mut hasher);
        0u8.hash(&mut hasher);
    }
    for key in cfg.env.keys() {
        key.hash(&mut hasher);
        0u8.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

pub struct McpPool {
    servers: BTreeMap<String, McpServerConfig>,
    cache_dir: CacheDir,
    cwd: Option<PathBuf>,
    clients: tokio::sync::Mutex<HashMap<String, Arc<McpClient>>>,
    schema_cache: std::sync::Mutex<HashMap<String, Vec<McpToolInfo>>>,
}

impl McpPool {
    pub fn new(
        servers: BTreeMap<String, McpServerConfig>,
        cache_dir: CacheDir,
        cwd: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            servers,
            cache_dir,
            cwd,
            clients: tokio::sync::Mutex::new(HashMap::new()),
            schema_cache: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    fn load_disk_cache(&self, server: &str, cfg: &McpServerConfig) -> Option<Vec<McpToolInfo>> {
        let cache_dir = self.cache_dir.as_ref()?;
        let path = cache_dir.join(format!("{server}.json"));
        let content = std::fs::read_to_string(&path).ok()?;
        let entry: DiskCacheEntry = serde_json::from_str(&content).ok()?;
        if entry.fingerprint == fingerprint(cfg) {
            let mut mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.insert(server.to_string(), entry.tools.clone());
            Some(entry.tools)
        } else {
            None
        }
    }

    fn save_cache(&self, server: &str, cfg: &McpServerConfig, tools: &[McpToolInfo]) {
        {
            let mut mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.insert(server.to_string(), tools.to_vec());
        }

        if let Some(cache_dir) = &self.cache_dir {
            if let Err(err) = std::fs::create_dir_all(cache_dir) {
                log::warn!(
                    "Failed to create schema cache dir {}: {err}",
                    cache_dir.display()
                );
                return;
            }
            let path = cache_dir.join(format!("{server}.json"));
            let entry = DiskCacheEntry {
                fingerprint: fingerprint(cfg),
                tools: tools.to_vec(),
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
    }

    fn get_cached_tools(&self, server: &str, cfg: &McpServerConfig) -> Option<Vec<McpToolInfo>> {
        let in_mem = {
            let mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(server).cloned()
        };
        in_mem.or_else(|| self.load_disk_cache(server, cfg))
    }

    async fn get_or_start_client(&self, server: &str) -> Result<Arc<McpClient>, McpError> {
        let cfg = self
            .servers
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.to_string()))?;

        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(server) {
            if client.is_alive() {
                return Ok(Arc::clone(client));
            }
            clients.remove(server);
        }

        let client = McpClient::start(server, cfg, self.cwd.as_deref())
            .await
            .map_err(|e| match e {
                McpError::Spawn { server, source } => McpError::Spawn { server, source },
                other => McpError::Spawn {
                    server: server.to_string(),
                    source: std::io::Error::other(other.to_string()),
                },
            })?;
        let client = Arc::new(client);
        clients.insert(server.to_string(), Arc::clone(&client));
        Ok(client)
    }

    /// Tools of every server: non-lazy servers are started, lazy ones answer from the
    /// schema cache when present (else started once to fill it).
    pub async fn list_all_tools(&self) -> Vec<(String, McpToolInfo)> {
        let mut all_tools = Vec::new();

        for (server_name, cfg) in &self.servers {
            let cached = self.get_cached_tools(server_name, cfg);
            let tools = if cfg.lazy {
                if let Some(tools) = cached {
                    tools
                } else {
                    match self.list_tools(server_name).await {
                        Ok(tools) => tools,
                        Err(err) => {
                            log::warn!("Server '{server_name}' failed to start: {err}");
                            continue;
                        }
                    }
                }
            } else {
                match self.list_tools(server_name).await {
                    Ok(tools) => tools,
                    Err(err) => {
                        log::warn!("Server '{server_name}' failed to start: {err}");
                        continue;
                    }
                }
            };

            for tool in tools {
                all_tools.push((server_name.clone(), tool));
            }
        }

        all_tools.sort_by(|a, b| {
            (a.0.as_str(), a.1.name.as_str()).cmp(&(b.0.as_str(), b.1.name.as_str()))
        });

        all_tools
    }

    pub async fn list_tools(&self, server: &str) -> Result<Vec<McpToolInfo>, McpError> {
        let cfg = self
            .servers
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.to_string()))?;
        let client = self.get_or_start_client(server).await?;
        let tools = client.list_tools().await?;
        self.save_cache(server, cfg, &tools);
        Ok(tools)
    }

    /// Start the server if needed and call the tool with the server's `timeout_secs`.
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<McpCallResult, McpError> {
        let cfg = self
            .servers
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.to_string()))?;
        let timeout = Duration::from_secs(if cfg.timeout_secs == 0 {
            60
        } else {
            cfg.timeout_secs
        });

        let client = self.get_or_start_client(server).await?;
        let res = client.call_tool(tool, args.clone(), timeout).await;

        let is_dead = match &res {
            Err(McpError::Closed) => true,
            Err(_) if !client.is_alive() => true,
            _ => !client.is_alive(),
        };

        if is_dead && res.is_err() {
            {
                let mut clients = self.clients.lock().await;
                clients.remove(server);
            }
            let fresh_client = self.get_or_start_client(server).await?;
            fresh_client.call_tool(tool, args, timeout).await
        } else {
            res
        }
    }

    pub async fn shutdown(&self) {
        let clients: Vec<Arc<McpClient>> = {
            let mut map = self.clients.lock().await;
            map.drain().map(|(_, c)| c).collect()
        };
        for client in clients {
            client.shutdown().await;
        }
    }
}
