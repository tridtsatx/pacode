//! Server pool with lazy start, schema cache, status tracking, and sampling support.

#[cfg(test)]
#[path = "pool_tests.rs"]
mod pool_tests;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use pacode_types::McpServerConfig;
use serde_json::Value;

use crate::cache::{ServerSchemaCache, load_disk_cache, save_disk_cache};
use crate::{
    CacheDir, McpCallResult, McpClient, McpError, McpPrompt, McpResource, McpToolInfo,
    SamplingHandler, ServerStatus,
};

struct PooledClient {
    client: Arc<McpClient>,
    last_used: std::sync::Mutex<std::time::Instant>,
    in_flight: AtomicUsize,
}

pub struct InFlightGuard {
    pooled: Arc<PooledClient>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut lu) = self.pooled.last_used.lock() {
            *lu = std::time::Instant::now();
        }
        self.pooled.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct McpPool {
    servers: std::sync::RwLock<BTreeMap<String, McpServerConfig>>,
    statuses: std::sync::RwLock<HashMap<String, ServerStatus>>,
    cache_dir: CacheDir,
    cwd: Option<PathBuf>,
    clients: tokio::sync::Mutex<HashMap<String, Arc<PooledClient>>>,
    schema_cache: std::sync::Mutex<HashMap<String, ServerSchemaCache>>,
    sampling_handler: Arc<std::sync::RwLock<Option<Arc<dyn SamplingHandler>>>>,
    sampling_enabled: Arc<AtomicBool>,
    sampling_max_tokens: Arc<AtomicU32>,
    idle_timeout_ms: Arc<AtomicU64>,
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
            idle_timeout_ms: Arc::new(AtomicU64::new(300_000)),
        })
    }

    pub fn set_idle_timeout_secs(&self, secs: u64) {
        self.idle_timeout_ms
            .store(secs.saturating_mul(1000), Ordering::Relaxed);
    }

    pub fn set_idle_timeout(&self, duration: Duration) {
        self.idle_timeout_ms
            .store(duration.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn idle_timeout_secs(&self) -> u64 {
        self.idle_timeout_ms.load(Ordering::Relaxed) / 1000
    }

    pub fn is_running(&self, server: &str) -> bool {
        if let Ok(clients) = self.clients.try_lock() {
            clients.contains_key(server)
        } else {
            false
        }
    }

    pub async fn is_running_async(&self, server: &str) -> bool {
        self.clients.lock().await.contains_key(server)
    }

    pub async fn reap_idle(&self) -> Vec<String> {
        let timeout_ms = self.idle_timeout_ms.load(Ordering::Relaxed);
        if timeout_ms == 0 {
            return Vec::new();
        }
        let timeout = Duration::from_millis(timeout_ms);
        let now = std::time::Instant::now();

        let to_reap: Vec<(String, Arc<McpClient>)> = {
            let mut clients = self.clients.lock().await;
            let mut reaped = Vec::new();
            clients.retain(|name, pooled| {
                if pooled.in_flight.load(Ordering::SeqCst) > 0 {
                    return true;
                }
                let last_used = pooled.last_used.lock().map(|l| *l).unwrap_or(now);
                if now.duration_since(last_used) >= timeout {
                    reaped.push((name.clone(), Arc::clone(&pooled.client)));
                    false
                } else {
                    true
                }
            });
            reaped
        };

        let mut names = Vec::new();
        for (name, client) in to_reap {
            log::info!("reaping idle MCP server '{name}'");
            client.shutdown().await;
            self.update_status_ready(&name);
            names.push(name);
        }
        names
    }

    fn try_reap_idle_sync(&self) {
        let timeout_ms = self.idle_timeout_ms.load(Ordering::Relaxed);
        if timeout_ms == 0 {
            return;
        }
        let timeout = Duration::from_millis(timeout_ms);
        let now = std::time::Instant::now();

        let Ok(mut clients) = self.clients.try_lock() else {
            return;
        };

        let mut to_reap = Vec::new();
        clients.retain(|name, pooled| {
            if pooled.in_flight.load(Ordering::SeqCst) > 0 {
                return true;
            }
            let last_used = pooled.last_used.lock().map(|l| *l).unwrap_or(now);
            if now.duration_since(last_used) >= timeout {
                to_reap.push((name.clone(), Arc::clone(&pooled.client)));
                false
            } else {
                true
            }
        });

        for (name, client) in to_reap {
            log::info!("reaping idle MCP server '{name}' (sync)");
            self.update_status_ready(&name);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    client.shutdown().await;
                });
            }
        }
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
            for pooled in clients.values() {
                pooled.client.set_sampling_handler(Arc::clone(&handler));
            }
        }
    }

    pub fn set_sampling_config(&self, enabled: bool, max_tokens: u32) {
        self.sampling_enabled.store(enabled, Ordering::Relaxed);
        self.sampling_max_tokens
            .store(max_tokens, Ordering::Relaxed);
        if let Ok(clients) = self.clients.try_lock() {
            for pooled in clients.values() {
                pooled.client.set_sampling_config(enabled, max_tokens);
            }
        }
    }

    pub fn statuses(&self) -> Vec<(String, ServerStatus)> {
        self.try_reap_idle_sync();
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
                            if !c.client.is_alive() {
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
        self.reap_idle().await;
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
                clients.remove(name).map(|p| Arc::clone(&p.client))
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
        self.reap_idle().await;
        let cfg = self.server_config(server)?;
        {
            let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
            statuses.insert(server.to_string(), ServerStatus::Starting);
        }

        let old_client = {
            let mut clients = self.clients.lock().await;
            clients.remove(server).map(|p| Arc::clone(&p.client))
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
            let pooled = Arc::new(PooledClient {
                client: Arc::clone(&client),
                last_used: std::sync::Mutex::new(std::time::Instant::now()),
                in_flight: AtomicUsize::new(0),
            });
            clients.insert(server.to_string(), pooled);
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
            log::debug!("MCP schema cache memory hit for '{server}'");
            return Some(cache.clone());
        }
        if let Some(cache_dir) = &self.cache_dir {
            let disk_result = load_disk_cache(cache_dir, server, cfg, &mut mem);
            if disk_result.is_some() {
                log::debug!("MCP schema cache disk hit for '{server}'");
            } else {
                log::debug!("MCP schema cache miss for '{server}'");
            }
            disk_result
        } else {
            log::debug!("MCP schema cache miss for '{server}' (no cache dir)");
            None
        }
    }

    async fn start_client_internal(
        &self,
        server: &str,
        cfg: &McpServerConfig,
    ) -> Result<Arc<McpClient>, McpError> {
        log::info!("starting MCP server '{server}'");
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
        .map_err(|e| {
            log::warn!("failed to start MCP server '{server}': {e}");
            match e {
                McpError::Spawn { server, source } => McpError::Spawn { server, source },
                other => McpError::Spawn {
                    server: server.to_string(),
                    source: std::io::Error::other(other.to_string()),
                },
            }
        })?;

        log::info!("MCP server '{server}' started successfully");
        Ok(Arc::new(client))
    }

    pub async fn get_or_start_client(&self, server: &str) -> Result<Arc<McpClient>, McpError> {
        self.reap_idle().await;
        let pooled = self.get_or_start_pooled_client(server).await?;
        Ok(Arc::clone(&pooled.client))
    }

    pub async fn acquire_client(
        &self,
        server: &str,
    ) -> Result<(Arc<McpClient>, InFlightGuard), McpError> {
        self.reap_idle().await;
        let pooled = self.get_or_start_pooled_client(server).await?;
        pooled.in_flight.fetch_add(1, Ordering::SeqCst);
        let client = Arc::clone(&pooled.client);
        let guard = InFlightGuard { pooled };
        Ok((client, guard))
    }

    async fn get_or_start_pooled_client(
        &self,
        server: &str,
    ) -> Result<Arc<PooledClient>, McpError> {
        let cfg = self.server_config(server)?;
        let mut clients = self.clients.lock().await;
        if let Some(pooled) = clients.get(server) {
            if pooled.client.is_alive() {
                return Ok(Arc::clone(pooled));
            }
            clients.remove(server);
        }

        {
            let mut statuses = self.statuses.write().unwrap_or_else(|p| p.into_inner());
            statuses.insert(server.to_string(), ServerStatus::Starting);
        }

        match self.start_client_internal(server, &cfg).await {
            Ok(client) => {
                let pooled = Arc::new(PooledClient {
                    client,
                    last_used: std::sync::Mutex::new(std::time::Instant::now()),
                    in_flight: AtomicUsize::new(0),
                });
                clients.insert(server.to_string(), Arc::clone(&pooled));
                Ok(pooled)
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
        self.reap_idle().await;
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
        let (client, guard) = self.acquire_client(server).await?;
        let tools_res = client.list_tools().await;
        drop(guard);
        let tools = tools_res?;
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
        self.reap_idle().await;
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
        let (client, guard) = self.acquire_client(server).await?;
        let res_result = client.list_resources().await;
        drop(guard);
        let resources = res_result?;
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
        self.reap_idle().await;
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
        let (client, guard) = self.acquire_client(server).await?;
        let prompts_res = client.list_prompts().await;
        drop(guard);
        let prompts = prompts_res?;
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
        let (client, guard) = self.acquire_client(server).await?;
        let res = f(Arc::clone(&client)).await;
        drop(guard);
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
            let (fresh, guard) = self.acquire_client(server).await?;
            let res = f(fresh).await;
            drop(guard);
            res
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
            map.drain().map(|(_, p)| Arc::clone(&p.client)).collect()
        };
        for client in clients {
            client.shutdown().await;
        }
    }
}
