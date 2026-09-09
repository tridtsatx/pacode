//! Tests for MCP Streamable HTTP transport against an in-process TcpListener server.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use pacode_mcp::{
    McpClient, McpError, McpPool, SamplingHandler, SamplingRequest, SamplingResponse, ServerStatus,
};
use pacode_types::McpServerConfig;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct HttpRequest {
    pub _method: String,
    pub _path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Result<HttpRequest, std::io::Error> {
    let mut buf = Vec::new();
    let mut temp = [0u8; 1024];
    let header_end;

    loop {
        let n = stream.read(&mut temp).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof while reading headers",
            ));
        }
        buf.extend_from_slice(&temp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
    }

    let header_bytes = &buf[..header_end - 4];
    let header_str = String::from_utf8_lossy(header_bytes);
    let mut lines = header_str.lines();
    let req_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = req_line.split_whitespace().collect();
    let _method = parts.first().copied().unwrap_or("").to_string();
    let _path = parts.get(1).copied().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let content_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let body_start = header_end;
    while buf.len() - body_start < content_len {
        let n = stream.read(&mut temp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);
    }

    let body_bytes = &buf[body_start..body_start + (buf.len() - body_start).min(content_len)];
    let body = String::from_utf8_lossy(body_bytes).to_string();

    Ok(HttpRequest {
        _method,
        _path,
        headers,
        body,
    })
}

async fn send_json_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    session_id: Option<&str>,
    body: &str,
) -> Result<(), std::io::Error> {
    let mut header_str = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(sid) = session_id {
        header_str.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
    }
    header_str.push_str("\r\n");

    stream.write_all(header_str.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn send_sse_response(
    stream: &mut tokio::net::TcpStream,
    session_id: Option<&str>,
    sse_body: &str,
) -> Result<(), std::io::Error> {
    let mut header_str = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n",
        sse_body.len()
    );
    if let Some(sid) = session_id {
        header_str.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
    }
    header_str.push_str("\r\n");

    stream.write_all(header_str.as_bytes()).await?;
    stream.write_all(sse_body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

struct TestServer {
    pub url: String,
    pub session_echo_verified: Arc<AtomicBool>,
    pub sampling_verified: Arc<AtomicBool>,
    pub _shutdown_tx: Option<oneshot::Sender<()>>,
}

async fn run_test_server() -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let port = listener.local_addr().expect("local addr").port();
    let url = format!("http://127.0.0.1:{port}/mcp");

    let session_echo_verified = Arc::new(AtomicBool::new(false));
    let sampling_verified = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let echo_flag = Arc::clone(&session_echo_verified);
    let sample_flag = Arc::clone(&sampling_verified);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    break;
                }
                accept_res = listener.accept() => {
                    let (mut stream, _) = match accept_res {
                        Ok(s) => s,
                        Err(_) => break,
                    };

                    let echo_flag = Arc::clone(&echo_flag);
                    let sample_flag = Arc::clone(&sample_flag);

                    tokio::spawn(async move {
                        let req = match read_request(&mut stream).await {
                            Ok(r) => r,
                            Err(_) => return,
                        };

                        // Check session echo if present
                        if let Some(sid) = req.headers.get("mcp-session-id")
                            && sid == "test-session-42"
                        {
                            echo_flag.store(true, Ordering::SeqCst);
                        }

                        let val: Value = match serde_json::from_str(&req.body) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = send_json_response(&mut stream, 400, None, "bad json").await;
                                return;
                            }
                        };

                        let method = val.get("method").and_then(|m| m.as_str()).unwrap_or("");
                        let req_id = val.get("id").cloned().unwrap_or(Value::Null);

                        // If this is a sampling reply from client (has id and result, no method)
                        if val.get("method").is_none() && val.get("id").is_some() {
                            if let Some(result) = val.get("result")
                                && result.get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str()) == Some("http fake answer")
                            {
                                sample_flag.store(true, Ordering::SeqCst);
                            }
                            let _ = send_json_response(&mut stream, 200, Some("test-session-42"), "{}").await;
                            return;
                        }

                        match method {
                            "initialize" => {
                                let body = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "protocolVersion": "2025-06-18",
                                        "capabilities": {
                                            "tools": {},
                                            "resources": {},
                                            "prompts": {},
                                            "sampling": {}
                                        },
                                        "serverInfo": {
                                            "name": "http-test-server",
                                            "version": "1.0.0"
                                        }
                                    }
                                }).to_string();
                                let _ = send_json_response(&mut stream, 200, Some("test-session-42"), &body).await;
                            }
                            "notifications/initialized" => {
                                let _ = send_json_response(&mut stream, 200, Some("test-session-42"), "{}").await;
                            }
                            "tools/list" => {
                                // Return tools as standard JSON
                                let body = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "tools": [
                                            {
                                                "name": "json_echo",
                                                "description": "Echoes back over JSON",
                                                "inputSchema": { "type": "object" }
                                            },
                                            {
                                                "name": "trigger_sample",
                                                "description": "Triggers sampling over SSE",
                                                "inputSchema": { "type": "object" }
                                            }
                                        ]
                                    }
                                }).to_string();
                                let _ = send_json_response(&mut stream, 200, Some("test-session-42"), &body).await;
                            }
                            "tools/call" => {
                                let tool_name = val.get("params").and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or("");
                                if tool_name == "trigger_sample" {
                                    // Send sampling request via SSE event first, then result
                                    let sample_req = json!({
                                        "jsonrpc": "2.0",
                                        "id": 9999,
                                        "method": "sampling/createMessage",
                                        "params": {
                                            "messages": [
                                                {
                                                    "role": "user",
                                                    "content": {
                                                        "type": "text",
                                                        "text": "http query"
                                                    }
                                                }
                                            ],
                                            "maxTokens": 4096
                                        }
                                    }).to_string();

                                    let final_resp = json!({
                                        "jsonrpc": "2.0",
                                        "id": req_id,
                                        "result": {
                                            "content": [
                                                { "type": "text", "text": "sampling-initiated" }
                                            ],
                                            "isError": false
                                        }
                                    }).to_string();

                                    let sse_stream = format!(
                                        "event: message\r\ndata: {sample_req}\r\n\r\nevent: message\r\ndata: {final_resp}\r\n\r\n"
                                    );
                                    let _ = send_sse_response(&mut stream, Some("test-session-42"), &sse_stream).await;
                                } else {
                                    let args = val.get("params").and_then(|p| p.get("arguments")).cloned().unwrap_or_else(|| json!({}));
                                    let body = json!({
                                        "jsonrpc": "2.0",
                                        "id": req_id,
                                        "result": {
                                            "content": [
                                                { "type": "text", "text": args.to_string() }
                                            ],
                                            "isError": false
                                        }
                                    }).to_string();
                                    let _ = send_json_response(&mut stream, 200, Some("test-session-42"), &body).await;
                                }
                            }
                            "resources/list" => {
                                // Return resources as SSE stream!
                                let res_json = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "resources": [
                                            {
                                                "uri": "http://doc1",
                                                "name": "Doc 1",
                                                "description": "SSE Resource Document",
                                                "mimeType": "text/plain"
                                            }
                                        ]
                                    }
                                }).to_string();
                                let sse_body = format!("event: message\r\ndata: {res_json}\r\n\r\n");
                                let _ = send_sse_response(&mut stream, Some("test-session-42"), &sse_body).await;
                            }
                            "resources/read" => {
                                let uri = val.get("params").and_then(|p| p.get("uri")).and_then(|u| u.as_str()).unwrap_or("");
                                let read_json = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "contents": [
                                            {
                                                "uri": uri,
                                                "mimeType": "text/plain",
                                                "text": format!("content of {uri} via SSE")
                                            }
                                        ]
                                    }
                                }).to_string();
                                let sse_body = format!("data: {read_json}\r\n\r\n");
                                let _ = send_sse_response(&mut stream, Some("test-session-42"), &sse_body).await;
                            }
                            "prompts/list" => {
                                let body = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "prompts": [
                                            {
                                                "name": "review_code",
                                                "description": "Code review prompt",
                                                "arguments": [
                                                    {
                                                        "name": "lang",
                                                        "description": "Language",
                                                        "required": true
                                                    }
                                                ]
                                            }
                                        ]
                                    }
                                }).to_string();
                                let _ = send_json_response(&mut stream, 200, Some("test-session-42"), &body).await;
                            }
                            "prompts/get" => {
                                let lang = val.get("params").and_then(|p| p.get("arguments")).and_then(|a| a.get("lang")).and_then(|l| l.as_str()).unwrap_or("rust");
                                let body = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "result": {
                                        "description": "Review prompt",
                                        "messages": [
                                            {
                                                "role": "user",
                                                "content": {
                                                    "type": "text",
                                                    "text": format!("Review my {lang} code please")
                                                }
                                            }
                                        ]
                                    }
                                }).to_string();
                                let _ = send_json_response(&mut stream, 200, Some("test-session-42"), &body).await;
                            }
                            _ => {
                                let body = json!({
                                    "jsonrpc": "2.0",
                                    "id": req_id,
                                    "error": {
                                        "code": -32601,
                                        "message": "Method not found"
                                    }
                                }).to_string();
                                let _ = send_json_response(&mut stream, 200, Some("test-session-42"), &body).await;
                            }
                        }
                    });
                }
            }
        }
    });

    TestServer {
        url,
        session_echo_verified,
        sampling_verified,
        _shutdown_tx: Some(shutdown_tx),
    }
}

struct HttpSamplingHandler {
    pub received_tokens: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl SamplingHandler for HttpSamplingHandler {
    async fn create_message(&self, req: SamplingRequest) -> Result<SamplingResponse, McpError> {
        if let Some(toks) = req.max_tokens {
            self.received_tokens.store(toks, Ordering::SeqCst);
        }
        Ok(SamplingResponse::text("test-model", "http fake answer"))
    }
}

#[tokio::test]
async fn test_http_transport_json_sse_session_and_features() {
    let server = run_test_server().await;

    let mut headers = BTreeMap::new();
    headers.insert("authorization".to_string(), "Bearer test-token".to_string());

    let cfg = McpServerConfig {
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        url: Some(server.url.clone()),
        headers,
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let sample_tokens = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(HttpSamplingHandler {
        received_tokens: Arc::clone(&sample_tokens),
    });

    let client =
        McpClient::start_with_sampling("http_server", &cfg, None, Some(handler), true, 2048)
            .await
            .expect("start http client");

    assert_eq!(client.name(), "http_server");
    assert_eq!(client.protocol_version(), "2025-06-18");

    // 1. JSON reply: tools/list
    let tools = client.list_tools().await.expect("list tools");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "json_echo");

    // Call tool via JSON
    let call_res = client
        .call_tool(
            "json_echo",
            json!({"hello": "world"}),
            Duration::from_secs(5),
        )
        .await
        .expect("call json_echo");
    assert!(!call_res.is_error);
    assert!(call_res.content.contains("world"));

    // Session ID echo must have been verified on subsequent requests
    assert!(server.session_echo_verified.load(Ordering::SeqCst));

    // 2. SSE reply: resources/list and resources/read
    let resources = client
        .list_resources()
        .await
        .expect("list resources via SSE");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "http://doc1");
    assert_eq!(resources[0].name, "Doc 1");

    let doc_content = client
        .read_resource("http://doc1")
        .await
        .expect("read resource via SSE");
    assert_eq!(doc_content, "content of http://doc1 via SSE");

    // 3. Prompts: prompts/list and prompts/get
    let prompts = client.list_prompts().await.expect("list prompts");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "review_code");

    let prompt_text = client
        .get_prompt("review_code", json!({"lang": "python"}))
        .await
        .expect("get prompt");
    assert_eq!(prompt_text, "Review my python code please");

    // 4. Sampling round-trip triggered via SSE
    let call_sample_res = client
        .call_tool("trigger_sample", json!({}), Duration::from_secs(5))
        .await
        .expect("trigger sample tool");
    assert!(!call_sample_res.is_error);
    assert_eq!(call_sample_res.content, "sampling-initiated");

    // Server must have received the sampling answer
    assert!(server.sampling_verified.load(Ordering::SeqCst));
    // Handler must have received capped tokens (4096 capped to 2048)
    assert_eq!(sample_tokens.load(Ordering::SeqCst), 2048);

    client.shutdown().await;
}

#[tokio::test]
async fn test_http_pool_status_transitions() {
    let server = run_test_server().await;

    let cfg = McpServerConfig {
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        url: Some(server.url.clone()),
        headers: BTreeMap::new(),
        lazy: true,
        timeout_secs: 5,
        ..Default::default()
    };

    let mut servers = BTreeMap::new();
    servers.insert("http_pool_srv".to_string(), cfg);
    let pool = McpPool::new(servers, None, None);

    // Initial status: Stopped
    let statuses = pool.statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].1, ServerStatus::Stopped);

    // Trigger lazy start via list_all_tools
    let tools = pool.list_all_tools().await;
    assert_eq!(tools.len(), 2);

    let statuses = pool.statuses();
    match &statuses[0].1 {
        ServerStatus::Ready { tools, .. } => {
            assert_eq!(*tools, 2);
        }
        other => panic!("expected Ready status, got {other:?}"),
    }

    // Disable server
    pool.set_enabled("http_pool_srv", false)
        .await
        .expect("disable");
    let statuses = pool.statuses();
    assert_eq!(statuses[0].1, ServerStatus::Stopped);

    // Disabled servers advertise nothing
    let tools = pool.list_all_tools().await;
    assert!(tools.is_empty());

    // Re-enable and restart
    pool.set_enabled("http_pool_srv", true)
        .await
        .expect("re-enable");
    pool.restart("http_pool_srv").await.expect("restart");

    let statuses = pool.statuses();
    match &statuses[0].1 {
        ServerStatus::Ready { tools, .. } => {
            assert_eq!(*tools, 2);
        }
        other => panic!("expected Ready status after restart, got {other:?}"),
    }

    pool.shutdown().await;
}
