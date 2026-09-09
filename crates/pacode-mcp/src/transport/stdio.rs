//! Stdio transport: launches server as a child process and communicates via JSON-RPC over stdin/stdout.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use pacode_types::McpServerConfig;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use super::super::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use super::super::{McpError, SamplingHandler, SamplingRequest};

pub struct StdioTransport {
    child: Mutex<Child>,
    writer: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicU64,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
    sampling_handler: Arc<std::sync::RwLock<Option<Arc<dyn SamplingHandler>>>>,
    sampling_enabled: Arc<AtomicBool>,
    sampling_max_tokens: Arc<AtomicU32>,
}

impl StdioTransport {
    pub async fn start(
        name: &str,
        cfg: &McpServerConfig,
        cwd: Option<&Path>,
        sampling_handler: Option<Arc<dyn SamplingHandler>>,
        sampling_enabled: bool,
        sampling_max_tokens: u32,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args);

        for (k, v) in std::env::vars() {
            cmd.env(k, v);
        }
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|source| McpError::Spawn {
            server: name.to_string(),
            source,
        })?;

        let stdin = child.stdin.take().ok_or_else(|| McpError::Spawn {
            server: name.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdin pipe unavailable"),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::Spawn {
            server: name.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stdout pipe unavailable"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| McpError::Spawn {
            server: name.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stderr pipe unavailable"),
        })?;

        let server_name = name.to_string();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("[MCP stderr {server_name}] {line}");
            }
        });

        let writer = Arc::new(Mutex::new(stdin));
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let reader_name = name.to_string();
        let pending_reader = Arc::clone(&pending);
        let writer_reader = Arc::clone(&writer);

        let handler_holder = Arc::new(std::sync::RwLock::new(sampling_handler));
        let enabled_holder = Arc::new(AtomicBool::new(sampling_enabled));
        let max_tokens_holder = Arc::new(AtomicU32::new(sampling_max_tokens));

        let handler_reader = Arc::clone(&handler_holder);
        let enabled_reader = Arc::clone(&enabled_holder);
        let max_tokens_reader = Arc::clone(&max_tokens_holder);

        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let val: Value = match serde_json::from_str(trimmed) {
                            Ok(v) => v,
                            Err(err) => {
                                log::debug!(
                                    "[MCP {reader_name}] malformed json ({err}): {trimmed}"
                                );
                                continue;
                            }
                        };

                        let method_opt = val.get("method").and_then(|m| m.as_str());
                        let id_val = val.get("id");
                        let has_id = id_val.is_some() && !id_val.is_some_and(|v| v.is_null());

                        if let Some(method) = method_opt {
                            if !has_id {
                                log::debug!("[MCP {reader_name}] notification: {trimmed}");
                            } else {
                                log::debug!("[MCP {reader_name}] server request: {trimmed}");
                                let req_id = id_val.cloned().unwrap_or(Value::Null);

                                if method == "sampling/createMessage" {
                                    let enabled = enabled_reader.load(Ordering::Relaxed);
                                    let maybe_handler = handler_reader
                                        .read()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .clone();

                                    let reply = match (enabled, maybe_handler) {
                                        (false, _) => JsonRpcResponse::error(
                                            req_id,
                                            -32601,
                                            "Sampling is disabled",
                                        ),
                                        (true, None) => JsonRpcResponse::error(
                                            req_id,
                                            -32601,
                                            "No sampling handler configured",
                                        ),
                                        (true, Some(handler)) => {
                                            let params = val
                                                .get("params")
                                                .cloned()
                                                .unwrap_or_else(|| serde_json::json!({}));
                                            match serde_json::from_value::<SamplingRequest>(params)
                                            {
                                                Ok(mut req) => {
                                                    let cap =
                                                        max_tokens_reader.load(Ordering::Relaxed);
                                                    let capped =
                                                        req.max_tokens.map_or(cap, |t| t.min(cap));
                                                    req.max_tokens = Some(capped);

                                                    match handler.create_message(req).await {
                                                        Ok(resp) => {
                                                            match serde_json::to_value(resp) {
                                                                Ok(res_val) => {
                                                                    JsonRpcResponse::success(
                                                                        req_id, res_val,
                                                                    )
                                                                }
                                                                Err(e) => JsonRpcResponse::error(
                                                                    req_id,
                                                                    -32603,
                                                                    format!(
                                                                        "serialization error: {e}"
                                                                    ),
                                                                ),
                                                            }
                                                        }
                                                        Err(e) => JsonRpcResponse::error(
                                                            req_id,
                                                            -32603,
                                                            e.to_string(),
                                                        ),
                                                    }
                                                }
                                                Err(e) => JsonRpcResponse::error(
                                                    req_id,
                                                    -32602,
                                                    format!("Invalid params: {e}"),
                                                ),
                                            }
                                        }
                                    };

                                    if let Ok(mut reply_str) = serde_json::to_string(&reply) {
                                        reply_str.push('\n');
                                        let mut w = writer_reader.lock().await;
                                        let _ = w.write_all(reply_str.as_bytes()).await;
                                        let _ = w.flush().await;
                                    }
                                } else {
                                    let err_reply =
                                        JsonRpcResponse::error(req_id, -32601, "Method not found");
                                    if let Ok(mut msg) = serde_json::to_string(&err_reply) {
                                        msg.push('\n');
                                        let mut w = writer_reader.lock().await;
                                        let _ = w.write_all(msg.as_bytes()).await;
                                        let _ = w.flush().await;
                                    }
                                }
                            }
                        } else if has_id
                            && (val.get("result").is_some() || val.get("error").is_some())
                        {
                            match serde_json::from_value::<JsonRpcResponse>(val) {
                                Ok(resp) => {
                                    let id_num = resp.id.as_u64().or_else(|| {
                                        resp.id.as_str().and_then(|s| s.parse::<u64>().ok())
                                    });
                                    if let Some(id_num) = id_num {
                                        let mut map = pending_reader.lock().await;
                                        if let Some(tx) = map.remove(&id_num) {
                                            let _ = tx.send(resp);
                                        } else {
                                            log::debug!(
                                                "[MCP {reader_name}] unexpected response id {id_num}"
                                            );
                                        }
                                    } else {
                                        log::debug!(
                                            "[MCP {reader_name}] response id not u64: {:?}",
                                            resp.id
                                        );
                                    }
                                }
                                Err(err) => {
                                    log::debug!(
                                        "[MCP {reader_name}] failed to parse response ({err}): {trimmed}"
                                    );
                                }
                            }
                        } else {
                            log::debug!("[MCP {reader_name}] ignored line: {trimmed}");
                        }
                    }
                    Ok(None) => {
                        log::debug!("[MCP {reader_name}] stdout EOF");
                        break;
                    }
                    Err(err) => {
                        log::debug!("[MCP {reader_name}] stdout read error: {err}");
                        break;
                    }
                }
            }
            let mut map = pending_reader.lock().await;
            map.clear();
        });

        Ok(Self {
            child: Mutex::new(child),
            writer,
            pending,
            next_id: AtomicU64::new(1),
            reader_task,
            stderr_task,
            sampling_handler: handler_holder,
            sampling_enabled: enabled_holder,
            sampling_max_tokens: max_tokens_holder,
        })
    }

    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, McpError> {
        if !self.is_alive() {
            return Err(McpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, tx);
        }

        let req = JsonRpcRequest::new(id, method, params);
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        {
            let mut w = self.writer.lock().await;
            if let Err(err) = w.write_all(line.as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(McpError::Io(err));
            }
            if let Err(err) = w.flush().await {
                self.pending.lock().await.remove(&id);
                return Err(McpError::Io(err));
            }
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => {
                if let Some(err) = resp.error {
                    Err(McpError::Server {
                        code: err.code,
                        message: err.message,
                    })
                } else {
                    Ok(resp)
                }
            }
            Ok(Err(_closed)) => Err(McpError::Closed),
            Err(_elapsed) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::Timeout(timeout.as_secs()))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let notif = JsonRpcNotification::new(method, params);
        let mut notif_str = serde_json::to_string(&notif)?;
        notif_str.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(notif_str.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    pub fn is_alive(&self) -> bool {
        !self.reader_task.is_finished()
    }

    pub fn set_sampling_handler(&self, handler: Arc<dyn SamplingHandler>) {
        let mut guard = self
            .sampling_handler
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *guard = Some(handler);
    }

    pub fn set_sampling_config(&self, enabled: bool, max_tokens: u32) {
        self.sampling_enabled.store(enabled, Ordering::Relaxed);
        self.sampling_max_tokens
            .store(max_tokens, Ordering::Relaxed);
    }

    pub async fn shutdown(&self) {
        self.reader_task.abort();
        self.stderr_task.abort();
        self.pending.lock().await.clear();
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.stderr_task.abort();
    }
}
