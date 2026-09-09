//! HTTP transport: Streamable HTTP (MCP spec 2025-06-18) over POST with SSE or JSON replies.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use pacode_types::McpServerConfig;
use serde_json::Value;

use super::super::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use super::super::{McpError, SamplingHandler, SamplingRequest};
use super::sse::parse_sse_events;

pub struct HttpTransport {
    url: String,
    headers: BTreeMap<String, String>,
    client: reqwest::Client,
    session_id: Arc<std::sync::RwLock<Option<String>>>,
    alive: Arc<AtomicBool>,
    next_id: AtomicU64,
    sampling_handler: Arc<std::sync::RwLock<Option<Arc<dyn SamplingHandler>>>>,
    sampling_enabled: Arc<AtomicBool>,
    sampling_max_tokens: Arc<AtomicU32>,
}

impl HttpTransport {
    pub async fn start(
        _name: &str,
        cfg: &McpServerConfig,
        sampling_handler: Option<Arc<dyn SamplingHandler>>,
        sampling_enabled: bool,
        sampling_max_tokens: u32,
    ) -> Result<Self, McpError> {
        let url = cfg
            .url
            .clone()
            .ok_or_else(|| McpError::Protocol("missing url in http server config".to_string()))?;

        let client = reqwest::Client::builder().build().map_err(McpError::Http)?;

        Ok(Self {
            url,
            headers: cfg.headers.clone(),
            client,
            session_id: Arc::new(std::sync::RwLock::new(None)),
            alive: Arc::new(AtomicBool::new(true)),
            next_id: AtomicU64::new(1),
            sampling_handler: Arc::new(std::sync::RwLock::new(sampling_handler)),
            sampling_enabled: Arc::new(AtomicBool::new(sampling_enabled)),
            sampling_max_tokens: Arc::new(AtomicU32::new(sampling_max_tokens)),
        })
    }

    fn check_session_id_header(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(val) = headers.get("mcp-session-id")
            && let Ok(sid) = val.to_str()
        {
            let mut guard = self.session_id.write().unwrap_or_else(|p| p.into_inner());
            *guard = Some(sid.to_string());
        }
    }

    async fn handle_server_request(&self, val: Value) -> Result<(), McpError> {
        let id_val = val.get("id").cloned().unwrap_or(Value::Null);
        let method = val.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let reply = if method == "sampling/createMessage" {
            let enabled = self.sampling_enabled.load(Ordering::Relaxed);
            let maybe_handler = self
                .sampling_handler
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone();

            match (enabled, maybe_handler) {
                (false, _) => JsonRpcResponse::error(id_val, -32601, "Sampling is disabled"),
                (true, None) => {
                    JsonRpcResponse::error(id_val, -32601, "No sampling handler configured")
                }
                (true, Some(handler)) => {
                    let params = val
                        .get("params")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    match serde_json::from_value::<SamplingRequest>(params) {
                        Ok(mut req) => {
                            let cap = self.sampling_max_tokens.load(Ordering::Relaxed);
                            let capped = req.max_tokens.map_or(cap, |t| t.min(cap));
                            req.max_tokens = Some(capped);

                            match handler.create_message(req).await {
                                Ok(resp) => match serde_json::to_value(resp) {
                                    Ok(res_val) => JsonRpcResponse::success(id_val, res_val),
                                    Err(e) => JsonRpcResponse::error(
                                        id_val,
                                        -32603,
                                        format!("serialization error: {e}"),
                                    ),
                                },
                                Err(e) => JsonRpcResponse::error(id_val, -32603, e.to_string()),
                            }
                        }
                        Err(e) => {
                            JsonRpcResponse::error(id_val, -32602, format!("Invalid params: {e}"))
                        }
                    }
                }
            }
        } else {
            JsonRpcResponse::error(id_val, -32601, "Method not found")
        };

        // POST reply back to url
        let mut req_builder = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");

        for (k, v) in &self.headers {
            req_builder = req_builder.header(k, v);
        }

        if let Some(sid) = self
            .session_id
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            req_builder = req_builder.header("mcp-session-id", sid);
        }

        let body = serde_json::to_string(&reply)?;
        let resp = req_builder.body(body).send().await?;
        self.check_session_id_header(resp.headers());
        Ok(())
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
        let req = JsonRpcRequest::new(id, method, params);
        let body = serde_json::to_string(&req)?;

        let mut req_builder = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");

        for (k, v) in &self.headers {
            req_builder = req_builder.header(k, v);
        }

        if let Some(sid) = self
            .session_id
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            req_builder = req_builder.header("mcp-session-id", sid);
        }

        let resp = tokio::time::timeout(timeout, req_builder.body(body).send())
            .await
            .map_err(|_| McpError::Timeout(timeout.as_secs()))?
            .map_err(McpError::Http)?;

        self.check_session_id_header(resp.headers());

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let resp_text = tokio::time::timeout(timeout, resp.text())
            .await
            .map_err(|_| McpError::Timeout(timeout.as_secs()))?
            .map_err(McpError::Http)?;

        if !status.is_success() {
            return Err(McpError::Protocol(format!(
                "HTTP error {status}: {resp_text}"
            )));
        }

        let is_sse = content_type.contains("text/event-stream")
            || resp_text.trim_start().starts_with("data:")
            || resp_text.trim_start().starts_with("event:");

        if is_sse {
            let events = parse_sse_events(&resp_text);
            let mut matched_response = None;

            for event in events {
                if event.data.is_empty() {
                    continue;
                }
                let val: Value = match serde_json::from_str(&event.data) {
                    Ok(v) => v,
                    Err(err) => {
                        log::debug!(
                            "failed to parse SSE event data as JSON ({err}): {}",
                            event.data
                        );
                        continue;
                    }
                };

                let has_method = val.get("method").and_then(|m| m.as_str()).is_some();
                let id_val = val.get("id");
                let has_id = id_val.is_some() && !id_val.is_some_and(|v| v.is_null());

                if has_method && has_id {
                    let _ = self.handle_server_request(val).await;
                } else if has_id
                    && (val.get("result").is_some() || val.get("error").is_some())
                    && let Ok(resp_obj) = serde_json::from_value::<JsonRpcResponse>(val)
                {
                    let id_num = resp_obj
                        .id
                        .as_u64()
                        .or_else(|| resp_obj.id.as_str().and_then(|s| s.parse::<u64>().ok()));
                    if id_num == Some(id) {
                        matched_response = Some(resp_obj);
                        break;
                    }
                }
            }

            if let Some(resp) = matched_response {
                if let Some(err) = resp.error {
                    Err(McpError::Server {
                        code: err.code,
                        message: err.message,
                    })
                } else {
                    Ok(resp)
                }
            } else {
                Err(McpError::Protocol(
                    "no matching response in SSE stream".to_string(),
                ))
            }
        } else {
            let resp_obj: JsonRpcResponse = serde_json::from_str(&resp_text)?;
            if let Some(err) = resp_obj.error {
                Err(McpError::Server {
                    code: err.code,
                    message: err.message,
                })
            } else {
                Ok(resp_obj)
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let notif = JsonRpcNotification::new(method, params);
        let body = serde_json::to_string(&notif)?;

        let mut req_builder = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");

        for (k, v) in &self.headers {
            req_builder = req_builder.header(k, v);
        }

        if let Some(sid) = self
            .session_id
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            req_builder = req_builder.header("mcp-session-id", sid);
        }

        let resp = req_builder.body(body).send().await?;
        self.check_session_id_header(resp.headers());
        Ok(())
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
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
        self.alive.store(false, Ordering::SeqCst);
    }
}
