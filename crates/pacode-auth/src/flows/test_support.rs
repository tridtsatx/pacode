use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// Start a single-request mock HTTP server on 127.0.0.1:0.
pub async fn mock_server(
    status: u16,
    response_body: &str,
) -> (u16, tokio::task::JoinHandle<RecordedRequest>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let response_body = response_body.to_string();

    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let recorded = read_request(&mut stream).await;

        let resp = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
        let _ = stream.flush().await;
        recorded
    });

    (port, handle)
}

/// Start a multi-request mock server that routes based on request target path.
pub async fn mock_server_multi_route(
    routes: HashMap<String, (u16, String)>,
    expected_requests: usize,
) -> (u16, tokio::task::JoinHandle<Vec<RecordedRequest>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        let mut recorded = Vec::new();
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            let req = read_request(&mut stream).await;

            let (status, body) = routes
                .get(&req.path)
                .cloned()
                .unwrap_or((404, r#"{"error":"not_found"}"#.to_string()));

            let resp = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
            let _ = stream.flush().await;
            recorded.push(req);
        }
        recorded
    });

    (port, handle)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> RecordedRequest {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut headers = HashMap::new();
    let mut method = String::new();
    let mut path = String::new();
    let mut content_length = 0;

    loop {
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);

        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let header_str = String::from_utf8_lossy(&buf[..pos]);
            let mut lines = header_str.lines();
            if let Some(first_line) = lines.next() {
                let mut parts = first_line.split_whitespace();
                method = parts.next().unwrap_or("").to_string();
                path = parts.next().unwrap_or("").to_string();
            }

            for line in lines {
                if let Some((k, v)) = line.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }

            if let Some(cl) = headers.get("content-length") {
                content_length = cl.parse::<usize>().unwrap_or(0);
            }

            let body_start = pos + 4;
            let mut body = buf[body_start..].to_vec();
            while body.len() < content_length {
                let n = stream.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }

            let body_str = String::from_utf8_lossy(&body).to_string();
            return RecordedRequest {
                method,
                path,
                headers,
                body: body_str,
            };
        }
    }

    RecordedRequest {
        method,
        path,
        headers,
        body: String::new(),
    }
}

pub fn create_mock_jwt(payload: &serde_json::Value) -> String {
    let header = serde_json::json!({"alg": "none", "typ": "JWT"});
    let h_b64 = URL_SAFE_NO_PAD.encode(header.to_string());
    let p_b64 = URL_SAFE_NO_PAD.encode(payload.to_string());
    format!("{h_b64}.{p_b64}.mock_signature")
}
