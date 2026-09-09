//! One MCP server process.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pacode_types::McpServerConfig;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, MCP_PROTOCOL_VERSION};
use crate::{McpCallResult, McpError, McpToolInfo};

pub struct McpClient {
    name: String,
    timeout_secs: u64,
    child: Mutex<Child>,
    writer: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicU64,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
}

impl McpClient {
    /// Spawn the server, run `initialize` + `notifications/initialized`.
    pub async fn start(
        name: &str,
        cfg: &McpServerConfig,
        cwd: Option<&std::path::Path>,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args);

        // cfg.env merged over the current env
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

                        let has_method = val.get("method").and_then(|m| m.as_str()).is_some();
                        let id_val = val.get("id");
                        let has_id = id_val.is_some() && !id_val.is_some_and(|v| v.is_null());

                        if has_method {
                            if !has_id {
                                log::debug!("[MCP {reader_name}] notification: {trimmed}");
                            } else {
                                log::debug!("[MCP {reader_name}] server request: {trimmed}");
                                let req_id = id_val.cloned().unwrap_or(Value::Null);
                                let err_reply = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "error": {
                                        "code": -32601,
                                        "message": "Method not found"
                                    }
                                });
                                let mut msg = err_reply.to_string();
                                msg.push('\n');
                                let mut w = writer_reader.lock().await;
                                let _ = w.write_all(msg.as_bytes()).await;
                                let _ = w.flush().await;
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

        let timeout_secs = if cfg.timeout_secs == 0 {
            60
        } else {
            cfg.timeout_secs
        };

        let client = Self {
            name: name.to_string(),
            timeout_secs,
            child: Mutex::new(child),
            writer,
            pending,
            next_id: AtomicU64::new(1),
            reader_task,
            stderr_task,
        };

        // Handshake: request `initialize` then notification `notifications/initialized`
        let init_params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "pacode",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let handshake_timeout = Duration::from_secs(timeout_secs);
        if let Err(err) = client
            .request("initialize", Some(init_params), handshake_timeout)
            .await
        {
            client.shutdown().await;
            return Err(err);
        }

        let notif = JsonRpcNotification::new("notifications/initialized", None);
        let mut notif_str = serde_json::to_string(&notif)?;
        notif_str.push('\n');
        {
            let mut w = client.writer.lock().await;
            if let Err(e) = w.write_all(notif_str.as_bytes()).await {
                client.shutdown().await;
                return Err(McpError::Io(e));
            }
            if let Err(e) = w.flush().await {
                client.shutdown().await;
                return Err(McpError::Io(e));
            }
        }

        Ok(client)
    }

    async fn request(
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

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let timeout = Duration::from_secs(self.timeout_secs);

        loop {
            let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
            let response = self.request("tools/list", params, timeout).await?;
            let result = response.result.ok_or_else(|| {
                McpError::Protocol("missing result in tools/list response".to_string())
            })?;

            if let Some(items) = result.get("tools").and_then(|t| t.as_array()) {
                for item in items {
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input_schema = item
                        .get("inputSchema")
                        .or_else(|| item.get("input_schema"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
                    tools.push(McpToolInfo {
                        name,
                        description,
                        input_schema,
                    });
                }
            }

            let next_cursor = result
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            match next_cursor {
                Some(nc) => cursor = Some(nc),
                None => break,
            }
        }

        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        tool: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<McpCallResult, McpError> {
        let arguments = if args.is_null() {
            serde_json::json!({})
        } else {
            args
        };
        let params = serde_json::json!({
            "name": tool,
            "arguments": arguments,
        });

        let response = self.request("tools/call", Some(params), timeout).await?;
        let result = response.result.ok_or_else(|| {
            McpError::Protocol("missing result in tools/call response".to_string())
        })?;

        let mut parts = Vec::new();
        if let Some(content_array) = result.get("content").and_then(|c| c.as_array()) {
            for block in content_array {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if block_type == "text" {
                    let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    parts.push(text.to_string());
                } else if !block_type.is_empty() {
                    parts.push(format!("[{block_type}]"));
                } else {
                    parts.push("[unknown]".to_string());
                }
            }
        } else if let Some(text) = result.get("content").and_then(|c| c.as_str()) {
            parts.push(text.to_string());
        }

        let content = parts.join("\n");
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(McpCallResult { content, is_error })
    }

    pub fn is_alive(&self) -> bool {
        !self.reader_task.is_finished()
    }

    /// Kill the process and stop the reader task.
    pub async fn shutdown(&self) {
        self.reader_task.abort();
        self.stderr_task.abort();
        self.pending.lock().await.clear();
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.stderr_task.abort();
    }
}
