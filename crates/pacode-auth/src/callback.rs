//! Ephemeral loopback HTTP OAuth callback server.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{AuthError, Result};

/// Parsed parameters returned from an OAuth authorization redirect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallbackParams {
    /// Authorization code granted by the provider.
    pub code: Option<String>,
    /// State token returned by the provider (used for CSRF validation).
    pub state: Option<String>,
    /// Error code or message if the provider rejected or cancelled authorization.
    pub error: Option<String>,
}

impl CallbackParams {
    /// Check whether authorization succeeded (code present, no error).
    pub fn is_success(&self) -> bool {
        self.code.is_some() && self.error.is_none()
    }

    /// Extract the authorization code or convert an error into [`AuthError`].
    pub fn into_result(self) -> Result<String> {
        if let Some(err) = self.error {
            return Err(AuthError::Denied(err));
        }
        self.code
            .ok_or_else(|| AuthError::Callback("missing authorization code in callback".into()))
    }
}

/// Loopback HTTP listener for receiving OAuth callbacks.
pub struct CallbackListener {
    listener: tokio::net::TcpListener,
    port: u16,
}

impl CallbackListener {
    /// Return the TCP port bound by this listener.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Build a loopback redirect URI for this listener on the given path (e.g. `"/callback"`).
    pub fn redirect_uri(&self, path: &str) -> String {
        let normalized = if path.starts_with('/') {
            path
        } else {
            &format!("/{path}")
        };
        format!("http://127.0.0.1:{}{normalized}", self.port)
    }

    /// Accept ONE HTTP request, parse the query string, and reply with a self-contained HTML page.
    ///
    /// The request's `state` parameter must match `expected_state`. If it does not,
    /// a failure page is served and [`AuthError::Callback`] is returned.
    /// If `timeout` expires before a connection is accepted, returns [`AuthError::Callback`].
    pub async fn wait(self, timeout: Duration, expected_state: &str) -> Result<CallbackParams> {
        tokio::time::timeout(timeout, self.wait_inner(expected_state))
            .await
            .map_err(|_| {
                AuthError::Callback(format!(
                    "timed out waiting for OAuth callback after {timeout:?}"
                ))
            })?
    }

    async fn wait_inner(self, expected_state: &str) -> Result<CallbackParams> {
        let (mut stream, _) = self.listener.accept().await.map_err(AuthError::Io)?;

        // Read the HTTP request until headers are terminated (\r\n\r\n or \n\n)
        let mut buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 1024];

        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && !buf.windows(2).any(|w| w == b"\n\n") {
            let n = stream.read(&mut chunk).await.map_err(AuthError::Io)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > 16 * 1024 {
                break;
            }
        }

        let text = String::from_utf8_lossy(&buf);
        let first_line = text.lines().next().unwrap_or("");
        let mut parts = first_line.split_whitespace();
        let _method = parts.next();
        let target = parts.next().unwrap_or("/");

        let query_string = target.split_once('?').map(|(_, q)| q).unwrap_or("");

        let mut code = None;
        let mut state = None;
        let mut error = None;

        for (k, v) in url::form_urlencoded::parse(query_string.as_bytes()) {
            match k.as_ref() {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                "error" => error = Some(v.into_owned()),
                _ => {}
            }
        }

        // Validate state
        if state.as_deref() != Some(expected_state) {
            let html = render_callback_html(
                false,
                "Authorization Failed",
                "State mismatch. For security reasons, the authorization request was rejected.",
            );
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;

            return Err(AuthError::Callback(format!(
                "OAuth state parameter mismatch: expected '{expected_state}', got '{:?}'",
                state.as_deref()
            )));
        }

        // State is valid: check for provider error vs success code
        if let Some(ref err) = error {
            let html = render_callback_html(
                false,
                "Authorization Failed",
                &format!(
                    "The authorization server returned an error: {err}. You may close this window."
                ),
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;

            return Ok(CallbackParams { code, state, error });
        }

        if code.is_some() {
            let html = render_callback_html(
                true,
                "Authorization Successful",
                "You are successfully authenticated! You can close this tab and return to pacode.",
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;

            return Ok(CallbackParams { code, state, error });
        }

        // Neither code nor error was provided
        let html = render_callback_html(
            false,
            "Authorization Incomplete",
            "No authorization code or error parameter was received. You may close this window.",
        );
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;

        Ok(CallbackParams { code, state, error })
    }
}

/// Bind a loopback callback server on `127.0.0.1:port` (0 = ephemeral).
///
/// If `preferred_port` is `Some(port)` (and >0), attempts to bind to that port first.
/// If unavailable, falls back to an ephemeral port (0).
pub fn bind_callback(preferred_port: Option<u16>) -> Result<CallbackListener> {
    let std_listener = match preferred_port {
        Some(port) if port > 0 => match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => listener,
            Err(err) => {
                tracing::warn!(
                    port,
                    error = %err,
                    "preferred callback port unavailable, falling back to ephemeral port"
                );
                std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| {
                    AuthError::Callback(format!("failed to bind loopback callback: {e}"))
                })?
            }
        },
        _ => std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| AuthError::Callback(format!("failed to bind loopback callback: {e}")))?,
    };

    std_listener
        .set_nonblocking(true)
        .map_err(|e| AuthError::Callback(format!("failed to set non-blocking: {e}")))?;

    let listener = tokio::net::TcpListener::from_std(std_listener)
        .map_err(|e| AuthError::Callback(format!("failed to register tokio listener: {e}")))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| AuthError::Callback(format!("failed to determine local addr: {e}")))?;

    Ok(CallbackListener {
        listener,
        port: local_addr.port(),
    })
}

fn render_callback_html(is_ok: bool, heading: &str, message: &str) -> String {
    let (badge_class, badge_icon) = if is_ok {
        ("badge-ok", "&#10003;")
    } else {
        ("badge-err", "&#10005;")
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{heading}</title>
<style>
:root {{
  --bg: #f9fafb;
  --card-bg: #ffffff;
  --text: #111827;
  --text-muted: #6b7280;
  --border: #e5e7eb;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --bg: #111827;
    --card-bg: #1f2937;
    --text: #f9fafb;
    --text-muted: #9ca3af;
    --border: #374151;
  }}
}}
body {{
  margin: 0;
  padding: 1.5rem;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  background: var(--bg);
  color: var(--text);
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 100vh;
  box-sizing: border-box;
}}
.card {{
  background: var(--card-bg);
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 2rem;
  max-width: 440px;
  width: 100%;
  text-align: center;
  box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1);
}}
h1 {{ font-size: 1.25rem; margin: 0 0 0.75rem 0; font-weight: 600; }}
p {{ font-size: 0.95rem; color: var(--text-muted); margin: 0; line-height: 1.5; }}
.badge {{
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 52px;
  height: 52px;
  border-radius: 50%;
  font-size: 24px;
  margin-bottom: 1.25rem;
}}
.badge-ok {{ background: rgba(16, 185, 129, 0.15); color: #10b981; }}
.badge-err {{ background: rgba(239, 68, 68, 0.15); color: #ef4444; }}
</style>
</head>
<body>
<div class="card">
  <div class="badge {badge_class}">{badge_icon}</div>
  <h1>{heading}</h1>
  <p>{message}</p>
</div>
</body>
</html>"#
    )
}

#[cfg(test)]
#[path = "callback_tests.rs"]
mod callback_tests;
