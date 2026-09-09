//! Server pool with lazy start, schema cache, status tracking, and sampling support.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use pacode_types::McpServerConfig;
use serde_json::Value;

use crate::cache::{ServerSchemaCache, load_disk_cache, save_disk_cache};
use crate::{
    CacheDir, McpCallResult, McpClient, McpError, McpPrompt, McpResource, McpToolInfo,
    SamplingHandler, ServerStatus,
};

pub struct McpPool {
    servers: std::sync::RwLock<BTreeMap<String, McpServerConfig>>,
    statuses: std::sync::RwLock<HashMap<String, ServerStatus>>,
    cache_dir: CacheDir,
    cwd: Option<PathBuf>,
    clients: tokio::sync::Mutex<HashMap<String, Arc<McpClient>>>,
    schema_cache: std::sync::Mutex<HashMap<String, ServerSchemaCache>>,
    sampling_handler: Arc<std::sync::RwLock<Option<Arc<dyn SamplingHandler>>>>,
    sampling_enabled: Arc<AtomicBool>,
    sampling_max_tokens: Arc<AtomicU32>,
}

impl McpPool {
    pub fn new(
        servers: BTreeMap<String, McpServerConfig>,
        cache_dir: CacheDir,
        cwd: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            servers: std::sync::RwLock::new(servers),
            statuses: std::sync::RwLock::new(HashMap::new()),
            cache_dir,
            cwd,
            clients: tokio::sync::Mutex::new(HashMap::new()),
            schema_cache: std::sync::Mutex::new(HashMap::new()),
            sampling_handler: Arc::new(std::sync::RwLock::new(None)),
            sampling_enabled: Arc::new(AtomicBool::new(true)),
            sampling_max_tokens: Arc::new(AtomicU32::new(2048)),
        })
    }

    pub fn server_names(&self) -> Vec<String> {
        self.servers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub fn is_enabled(&self, server: &str) -> bool {
        self.servers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(server)
            .is_some_and(|c| c.enabled)
    }

    pub fn set_sampling_handler(&self, handler: Arc<dyn SamplingHandler>) {
        {
            let mut guard = self
                .sampling_handler
                .write()
                .unwrap_or_else(|p| p.into_inner());
            *guard = Some(Arc::clone(&handler));
        }
        if let Ok(clients) = self.clients.try_lock() {
            for client in clients.values() {
                client.set_sampling_handler(Arc::clone(&handler));
            }
        }
    }

    pub fn set_sampling_config(&self, enabled: bool, max_tokens: u32) {
        self.sampling_enabled.store(enabled, Ordering::Relaxed);
        self.sampling_max_tokens
            .store(max_tokens, Ordering::Relaxed);
        if let Ok(clients) = self.clients.try_lock() {
            for client in clients.values() {
                client.set_sampling_config(enabled, max_tokens);
            }
        }
    }

    pub fn statuses(&self) -> Vec<(String, ServerStatus)> {
        let servers = self.servers.read().unwrap_or_else(|p| p.into_inner());
        let statuses = self.statuses.read().unwrap_or_else(|p| p.into_inner());
        let clients_guard = self.clients.try_lock().ok();

        let mut list = Vec::new();
        for (name, cfg) in servers.iter() {
            if !cfg.enabled {
                list.push((name.clone(), ServerStatus::Stopped));
                continue;
            }

            let status = statuses.get(name).cloned().unwrap_or(ServerStatus::Stopped);

            let effective_status = match &status {
                ServerStatus::Ready { .. } => {
                    if let Some(ref clients) = clients_guard {
                        if let Some(c) = clients.get(name) {
                            if !c.is_alive() {
                                ServerStatus::Stopped
                            } else {
                                status
                            }
                        } else {
                            status
                        }
                    } else {
                        status
                    }
                }
                _ => status,
            };

            list.push((name.clone(), effective_status));
        }

        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), McpError> {
        {
            let mut servers = self.servers.write().unwrap_or_else(|p| p.into_inner());
            let Some(cfg) = servers.get_mut(name) else {
                return Err(McpError::UnknownServer(name.to_string()));
            };
            cfg.enabled = enabled;
        }

        if !enabled {
            let client = {
                let mut clients = self.clients.lock().await;
                clients.remove(name)
            };
            if let Some(client) = client {
                client.shutdown().await;
            }
        }

        let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
        statuses.insert(name.to_string(), ServerStatus::Stopped);
        Ok(())
    }

    pub async fn restart(&self, server: &str) -> Result<(), McpError> {
        let cfg = self.server_config(server)?;
        {
            let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
            statuses.insert(server.to_string(), ServerStatus::Starting);
        }

        let old_client = {
            let mut clients = self.clients.lock().await;
            clients.remove(server)
        };
        if let Some(client) = old_client {
            client.shutdown().await;
        }

        let client = match self.start_client_internal(server, &cfg).await {
            Ok(c) => c,
            Err(err) => {
                let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
                statuses.insert(server.to_string(), ServerStatus::Failed(err.to_string()));
                return Err(err);
            }
        };

        {
            let mut clients = self.clients.lock().await;
            clients.insert(server.to_string(), Arc::clone(&client));
        }

        let tools = client.list_tools().await.unwrap_or_default();
        let resources = client.list_resources().await.unwrap_or_default();
        let prompts = client.list_prompts().await.unwrap_or_default();

        self.save_cache(server, &cfg, &tools, &resources, &prompts);
        self.update_status_ready(server);

        Ok(())
    }

    fn server_config(&self, server: &str) -> Result<McpServerConfig, McpError> {
        let servers = self.servers.read().unwrap_or_else(|p| p.into_inner());
        let cfg = servers
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.to_string()))?;
        if !cfg.enabled {
            return Err(McpError::ServerDisabled(server.to_string()));
        }
        Ok(cfg.clone())
    }

    fn all_server_configs(&self) -> Vec<(String, McpServerConfig)> {
        let guard = self.servers.read().unwrap_or_else(|p| p.into_inner());
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    fn save_cache(
        &self,
        server: &str,
        cfg: &McpServerConfig,
        tools: &[McpToolInfo],
        resources: &[McpResource],
        prompts: &[McpPrompt],
    ) {
        {
            let mut mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.insert(
                server.to_string(),
                ServerSchemaCache {
                    tools: tools.to_vec(),
                    resources: resources.to_vec(),
                    prompts: prompts.to_vec(),
                },
            );
        }

        if let Some(cache_dir) = &self.cache_dir {
            save_disk_cache(cache_dir, server, cfg, tools, resources, prompts);
        }
    }

    fn get_cached_schema(&self, server: &str, cfg: &McpServerConfig) -> Option<ServerSchemaCache> {
        let mut mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cache) = mem.get(server) {
            return Some(cache.clone());
        }
        if let Some(cache_dir) = &self.cache_dir {
            load_disk_cache(cache_dir, server, cfg, &mut mem)
        } else {
            None
        }
    }

    async fn start_client_internal(
        &self,
        server: &str,
        cfg: &McpServerConfig,
    ) -> Result<Arc<McpClient>, McpError> {
        let sampling_handler = self
            .sampling_handler
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let sampling_enabled = self.sampling_enabled.load(Ordering::Relaxed);
        let sampling_max_tokens = self.sampling_max_tokens.load(Ordering::Relaxed);

        let client = McpClient::start_with_sampling(
            server,
            cfg,
            self.cwd.as_deref(),
            sampling_handler,
            sampling_enabled,
            sampling_max_tokens,
        )
        .await
        .map_err(|e| match e {
            McpError::Spawn { server, source } => McpError::Spawn { server, source },
            other => McpError::Spawn {
                server: server.to_string(),
                source: std::io::Error::other(other.to_string()),
            },
        })?;

        Ok(Arc::new(client))
    }

    async fn get_or_start_client(&self, server: &str) -> Result<Arc<McpClient>, McpError> {
        let cfg = self.server_config(server)?;
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(server) {
            if client.is_alive() {
                return Ok(Arc::clone(client));
            }
            clients.remove(server);
        }

        {
            let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
            statuses.insert(server.to_string(), ServerStatus::Starting);
        }

        match self.start_client_internal(server, &cfg).await {
            Ok(client) => {
                clients.insert(server.to_string(), Arc::clone(&client));
                Ok(client)
            }
            Err(err) => {
                let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
                statuses.insert(server.to_string(), ServerStatus::Failed(err.to_string()));
                Err(err)
            }
        }
    }

    fn update_status_ready(&self, server: &str) {
        let cache = {
            let mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(server).cloned().unwrap_or_default()
        };
        let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
        statuses.insert(
            server.to_string(),
            ServerStatus::Ready {
                tools: cache.tools.len(),
                resources: cache.resources.len(),
                prompts: cache.prompts.len(),
            },
        );
    }

    pub async fn list_all_tools(&self) -> Vec<(String, McpToolInfo)> {
        let mut all = Vec::new();
        for (server, cfg) in self.all_server_configs() {
            if !cfg.enabled {
                continue;
            }
            let cached = self.get_cached_schema(&server, &cfg);
            let tools = if cfg.lazy {
                if let Some(schema) = cached {
                    self.update_status_ready(&server);
                    schema.tools
                } else {
                    self.list_tools(&server).await.unwrap_or_default()
                }
            } else {
                self.list_tools(&server).await.unwrap_or_default()
            };
            for tool in tools {
                all.push((server.clone(), tool));
            }
        }
        all.sort_by(|a, b| {
            (a.0.as_str(), a.1.name.as_str()).cmp(&(b.0.as_str(), b.1.name.as_str()))
        });
        all
    }

    pub async fn list_tools(&self, server: &str) -> Result<Vec<McpToolInfo>, McpError> {
        let cfg = self.server_config(server)?;
        let client = self.get_or_start_client(server).await?;
        let tools = client.list_tools().await?;
        let (resources, prompts) = {
            let mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(server)
                .map(|s| (s.resources.clone(), s.prompts.clone()))
                .unwrap_or_default()
        };
        self.save_cache(server, &cfg, &tools, &resources, &prompts);
        self.update_status_ready(server);
        Ok(tools)
    }

    pub async fn list_all_resources(&self) -> Vec<(String, McpResource)> {
        let mut all = Vec::new();
        for (server, cfg) in self.all_server_configs() {
            if !cfg.enabled {
                continue;
            }
            let cached = self.get_cached_schema(&server, &cfg);
            let items = if cfg.lazy {
                if let Some(s) = cached {
                    s.resources
                } else {
                    self.list_resources(&server).await.unwrap_or_default()
                }
            } else {
                self.list_resources(&server).await.unwrap_or_default()
            };
            for item in items {
                all.push((server.clone(), item));
            }
        }
        all.sort_by(|a, b| (a.0.as_str(), a.1.uri.as_str()).cmp(&(b.0.as_str(), b.1.uri.as_str())));
        all
    }

    pub async fn list_resources(&self, server: &str) -> Result<Vec<McpResource>, McpError> {
        let cfg = self.server_config(server)?;
        let client = self.get_or_start_client(server).await?;
        let resources = client.list_resources().await?;
        let (tools, prompts) = {
            let mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(server)
                .map(|s| (s.tools.clone(), s.prompts.clone()))
                .unwrap_or_default()
        };
        self.save_cache(server, &cfg, &tools, &resources, &prompts);
        self.update_status_ready(server);
        Ok(resources)
    }

    pub async fn list_all_prompts(&self) -> Vec<(String, McpPrompt)> {
        let mut all = Vec::new();
        for (server, cfg) in self.all_server_configs() {
            if !cfg.enabled {
                continue;
            }
            let cached = self.get_cached_schema(&server, &cfg);
            let items = if cfg.lazy {
                if let Some(s) = cached {
                    s.prompts
                } else {
                    self.list_prompts(&server).await.unwrap_or_default()
                }
            } else {
                self.list_prompts(&server).await.unwrap_or_default()
            };
            for item in items {
                all.push((server.clone(), item));
            }
        }
        all.sort_by(|a, b| {
            (a.0.as_str(), a.1.name.as_str()).cmp(&(b.0.as_str(), b.1.name.as_str()))
        });
        all
    }

    pub async fn list_prompts(&self, server: &str) -> Result<Vec<McpPrompt>, McpError> {
        let cfg = self.server_config(server)?;
        let client = self.get_or_start_client(server).await?;
        let prompts = client.list_prompts().await?;
        let (tools, resources) = {
            let mem = self.schema_cache.lock().unwrap_or_else(|p| p.into_inner());
            mem.get(server)
                .map(|s| (s.tools.clone(), s.resources.clone()))
                .unwrap_or_default()
        };
        self.save_cache(server, &cfg, &tools, &resources, &prompts);
        self.update_status_ready(server);
        Ok(prompts)
    }

    async fn with_retry<T, F, Fut>(&self, server: &str, f: F) -> Result<T, McpError>
    where
        F: Fn(Arc<McpClient>) -> Fut,
        Fut: std::future::Future<Output = Result<T, McpError>>,
    {
        let client = self.get_or_start_client(server).await?;
        let res = f(Arc::clone(&client)).await;
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
            let fresh = self.get_or_start_client(server).await?;
            f(fresh).await
        } else {
            res
        }
    }

    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<McpCallResult, McpError> {
        let cfg = self.server_config(server)?;
        let timeout = Duration::from_secs(if cfg.timeout_secs == 0 {
            60
        } else {
            cfg.timeout_secs
        });
        let tool = tool.to_string();
        self.with_retry(server, |c| {
            let tool = tool.clone();
            let args = args.clone();
            async move { c.call_tool(&tool, args, timeout).await }
        })
        .await
    }

    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<String, McpError> {
        let uri = uri.to_string();
        self.with_retry(server, |c| {
            let uri = uri.clone();
            async move { c.read_resource(&uri).await }
        })
        .await
    }

    pub async fn get_prompt(
        &self,
        server: &str,
        name: &str,
        args: Value,
    ) -> Result<String, McpError> {
        let name = name.to_string();
        self.with_retry(server, |c| {
            let name = name.clone();
            let args = args.clone();
            async move { c.get_prompt(&name, args).await }
        })
        .await
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
