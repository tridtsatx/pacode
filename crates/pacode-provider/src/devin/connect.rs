//! Connect-RPC transport for Devin API using protobuf payloads.
//!
//! Protocol facts:
//! - POST `{api_server_url}/{package}.{Service}/{Method}`
//! - Headers:
//!   - `content-type: application/proto` for unary calls
//!   - `content-type: application/connect+proto` for server-streaming calls
//!   - `connect-protocol-version: 1`
//!   - `authorization: Basic {api_key}-{session_token}` (literal dash concatenation, not base64)
//! - Unary RPCs return raw protobuf bytes.
//! - Streaming RPCs use Connect 5-byte envelope framing:
//!   - Byte 0: flags (bit 0x01 = compressed, bit 0x02 = end-of-stream trailer)
//!   - Bytes 1..5: length (u32 big-endian)
//!   - Payload: protobuf bytes for data frames, JSON `{}` or error for trailer frames.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::ProviderError;

/// Flag bit in Connect frame header indicating the trailer/end-of-stream message.
pub const CONNECT_FLAG_END_STREAM: u8 = 0x02;

/// Connect-RPC client communicating with the Devin API server.
#[derive(Clone, Debug)]
pub struct ConnectClient {
    client: reqwest::Client,
    api_server_url: String,
    api_key: String,
    session_token: String,
    custom_headers: Vec<(String, String)>,
}

impl ConnectClient {
    pub fn new(
        client: reqwest::Client,
        api_server_url: impl Into<String>,
        api_key: impl Into<String>,
        session_token: impl Into<String>,
        custom_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            client,
            api_server_url: api_server_url.into(),
            api_key: api_key.into(),
            session_token: session_token.into(),
            custom_headers,
        }
    }

    pub fn api_server_url(&self) -> &str {
        &self.api_server_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn session_token(&self) -> &str {
        &self.session_token
    }

    fn build_url(&self, method: &str) -> String {
        let path = method.strip_prefix('/').unwrap_or(method);
        format!("{}/{}", self.api_server_url.trim_end_matches('/'), path)
    }

    fn prepare_request(&self, url: &str, content_type: &str) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .post(url)
            .header("content-type", content_type)
            .header("connect-protocol-version", "1");

        let key = self.api_key.trim();
        let token = self.session_token.trim();
        if !key.is_empty() || !token.is_empty() {
            builder = builder.header("authorization", format!("Basic {key}-{token}"));
        }

        for (k, v) in &self.custom_headers {
            builder = builder.header(k, v);
        }

        builder
    }

    /// Perform a unary Connect-RPC call sending and receiving raw protobuf bytes.
    pub async fn unary(&self, method: &str, body: &[u8]) -> Result<Vec<u8>, ProviderError> {
        let url = self.build_url(method);
        let req_builder = self.prepare_request(&url, "application/proto");

        let response = req_builder
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let status_u16 = status.as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(parse_connect_error(&text, status_u16));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        Ok(bytes.to_vec())
    }

    /// Perform a server-streaming Connect-RPC call with optional idle timeout.
    pub async fn server_stream_with_idle_timeout(
        &self,
        method: &str,
        body: &[u8],
        idle_timeout: Option<(Duration, u64)>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>, ProviderError>
    {
        let url = self.build_url(method);
        let req_builder = self.prepare_request(&url, "application/connect+proto");

        let framed_body = encode_connect_frame(0x00, body);
        let response = req_builder
            .body(framed_body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let status_u16 = status.as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(parse_connect_error(&text, status_u16));
        }

        Ok(decode_response_stream(response, idle_timeout))
    }
}

// ---------------------------------------------------------------------------
// Frame Encoding and Decoding
// ---------------------------------------------------------------------------

/// Encode a Connect 5-byte framed payload.
pub fn encode_connect_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(flags);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// A decoded Connect frame: either data payload bytes or end-of-stream trailer.
#[derive(Debug)]
pub enum DecodedFrame {
    Data(Vec<u8>),
    End { error: Option<ProviderError> },
}

impl DecodedFrame {
    pub fn is_data(&self) -> bool {
        matches!(self, DecodedFrame::Data(_))
    }

    pub fn data(&self) -> Option<&[u8]> {
        match self {
            DecodedFrame::Data(v) => Some(v),
            DecodedFrame::End { .. } => None,
        }
    }

    pub fn is_end(&self) -> bool {
        matches!(self, DecodedFrame::End { .. })
    }

    pub fn end_error(&self) -> Option<&ProviderError> {
        match self {
            DecodedFrame::End { error } => error.as_ref(),
            DecodedFrame::Data(_) => None,
        }
    }
}

/// Incremental streaming decoder for Connect 5-byte framed streams.
#[derive(Default)]
pub struct ConnectFrameDecoder {
    buffer: Vec<u8>,
}

impl ConnectFrameDecoder {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feed byte chunks into the decoder and extract complete frames.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<DecodedFrame>, ProviderError> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();

        while self.buffer.len() >= 5 {
            let flags = self.buffer[0];
            let len = u32::from_be_bytes([
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
                self.buffer[4],
            ]) as usize;

            if self.buffer.len() < 5 + len {
                break;
            }

            let payload = self.buffer[5..5 + len].to_vec();
            self.buffer.drain(..5 + len);

            let is_end = (flags & CONNECT_FLAG_END_STREAM) != 0;
            if is_end {
                let error = if payload.is_empty() {
                    None
                } else {
                    match serde_json::from_slice::<Value>(&payload) {
                        Ok(json) => {
                            if let Some(err_obj) = json.get("error").filter(|v| !v.is_null()) {
                                let code = err_obj
                                    .get("code")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let message = err_obj
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Connect RPC error");
                                Some(map_connect_error_code(code, message.to_string(), 200))
                            } else if let Some(code) = json.get("code").and_then(|v| v.as_str()) {
                                let message = json
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Connect RPC error");
                                Some(map_connect_error_code(code, message.to_string(), 200))
                            } else {
                                None
                            }
                        }
                        Err(e) => {
                            return Err(ProviderError::Malformed(format!(
                                "invalid Connect trailer JSON: {e}"
                            )));
                        }
                    }
                };
                frames.push(DecodedFrame::End { error });
            } else {
                frames.push(DecodedFrame::Data(payload));
            }
        }

        Ok(frames)
    }

    /// Check for incomplete leftover bytes at stream end.
    pub fn finish(&self) -> Result<(), ProviderError> {
        if !self.buffer.is_empty() {
            Err(ProviderError::Malformed(
                "incomplete Connect frame at end of stream".to_string(),
            ))
        } else {
            Ok(())
        }
    }
}

fn decode_response_stream(
    response: reqwest::Response,
    idle_timeout: Option<(Duration, u64)>,
) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>> {
    struct StreamContext<S> {
        byte_stream: S,
        decoder: ConnectFrameDecoder,
        pending_frames: VecDeque<DecodedFrame>,
        pending_error: Option<ProviderError>,
        stream_ended: bool,
        idle_timeout: Option<(Duration, u64)>,
    }

    let initial = StreamContext {
        byte_stream: Box::pin(response.bytes_stream()),
        decoder: ConnectFrameDecoder::new(),
        pending_frames: VecDeque::new(),
        pending_error: None,
        stream_ended: false,
        idle_timeout,
    };

    let stream = futures::stream::unfold(initial, |mut ctx| async move {
        loop {
            if let Some(frame) = ctx.pending_frames.pop_front() {
                match frame {
                    DecodedFrame::Data(bytes) => return Some((Ok(bytes), ctx)),
                    DecodedFrame::End { error } => {
                        ctx.stream_ended = true;
                        if let Some(err) = error {
                            return Some((Err(err), ctx));
                        }
                        return None;
                    }
                }
            }

            if let Some(err) = ctx.pending_error.take() {
                ctx.stream_ended = true;
                return Some((Err(err), ctx));
            }

            if ctx.stream_ended {
                return None;
            }

            let next_item = if let Some((idle_duration, timeout_secs)) = ctx.idle_timeout {
                match tokio::time::timeout(idle_duration, ctx.byte_stream.next()).await {
                    Ok(item) => item,
                    Err(_) => {
                        ctx.stream_ended = true;
                        return Some((Err(ProviderError::IdleTimeout(timeout_secs)), ctx));
                    }
                }
            } else {
                ctx.byte_stream.next().await
            };

            match next_item {
                Some(Ok(bytes)) => match ctx.decoder.feed(&bytes) {
                    Ok(frames) => {
                        for frame in frames {
                            ctx.pending_frames.push_back(frame);
                        }
                    }
                    Err(err) => {
                        ctx.stream_ended = true;
                        return Some((Err(err), ctx));
                    }
                },
                Some(Err(err)) => {
                    ctx.stream_ended = true;
                    return Some((Err(ProviderError::Transport(err.to_string())), ctx));
                }
                None => {
                    ctx.stream_ended = true;
                    if let Err(err) = ctx.decoder.finish() {
                        return Some((Err(err), ctx));
                    }
                    return None;
                }
            }
        }
    });

    Box::pin(stream)
}

// ---------------------------------------------------------------------------
// Error Mapping
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ConnectErrorPayload {
    code: Option<String>,
    message: Option<String>,
}

/// Parse Connect RPC error response from HTTP status and response body.
pub fn parse_connect_error(body: &str, status: u16) -> ProviderError {
    if let Ok(err) = serde_json::from_str::<ConnectErrorPayload>(body)
        && let Some(code) = err.code
    {
        let message = err.message.unwrap_or_else(|| body.to_string());
        return map_connect_error_code(&code, message, status);
    }

    match status {
        401 | 403 => ProviderError::Auth(format!("{status}: {body}")),
        429 => ProviderError::RateLimited(body.to_string()),
        _ => ProviderError::Http {
            status,
            message: body.to_string(),
        },
    }
}

/// Map Connect RPC string codes to [`ProviderError`] variants.
pub fn map_connect_error_code(code: &str, message: String, fallback_status: u16) -> ProviderError {
    let lower = code.to_ascii_lowercase();
    match lower.as_str() {
        "unauthenticated" | "permission_denied" => ProviderError::Auth(message),
        "resource_exhausted" => ProviderError::RateLimited(message),
        "canceled" => ProviderError::Cancelled,
        "internal" | "data_loss" => ProviderError::Http {
            status: 500,
            message,
        },
        "unavailable" => ProviderError::Http {
            status: 503,
            message,
        },
        "deadline_exceeded" => ProviderError::Http {
            status: 504,
            message,
        },
        "invalid_argument" | "failed_precondition" | "out_of_range" => ProviderError::Http {
            status: 400,
            message,
        },
        "not_found" => ProviderError::Http {
            status: 404,
            message,
        },
        "already_exists" | "aborted" => ProviderError::Http {
            status: 409,
            message,
        },
        "unimplemented" => ProviderError::Http {
            status: 501,
            message,
        },
        _ => match fallback_status {
            401 | 403 => ProviderError::Auth(message),
            429 => ProviderError::RateLimited(message),
            s => ProviderError::Http { status: s, message },
        },
    }
}
