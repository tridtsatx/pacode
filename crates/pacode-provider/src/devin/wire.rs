//! Devin protobuf wire format request builders and response parsers.
//!
//! Protocol specifications captured from live Devin CLI traffic.

use pacode_types::{ContentBlock, Message, ProviderConfig, Role, ToolDefinition};

use super::proto::{self, ProtoError, WireType};
use crate::CompletionRequest;

pub const DEVIN_CLI_VERSION: &str = "3000.10.21";
pub const DEVIN_PRODUCT: &str = "chisel";

/// Identifier of the conversation, stable across every turn of it.
///
/// Field 16 of `GetChatMessage` is the conversation id, not a per-request id: the
/// official client keeps one value for a whole session, and an assignment token from
/// `AssignModel` is bound to it. `CompletionRequest` carries no session id, so the id
/// is derived from the opening of the conversation, which does not change as turns are
/// appended.
pub fn conversation_id(req: &CompletionRequest) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};

    let seed = req.messages.first().map(|m| m.text()).unwrap_or_default();
    let mut bytes = [0u8; 16];
    for (chunk, salt) in bytes.chunks_mut(8).zip([0x9e37_79b9u64, 0x85eb_ca6b]) {
        let mut hasher = DefaultHasher::new();
        salt.hash(&mut hasher);
        req.system_static.hash(&mut hasher);
        seed.hash(&mut hasher);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid_string(&bytes)
}

/// Generate a compliant RFC 4122 version 4 UUID string.
pub fn random_uuid() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid_string(&bytes)
}

fn uuid_string(bytes: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Encode the standard metadata submessage.
// NOTE: Field 31 is a client fingerprint we do not reproduce.
pub fn encode_metadata(session_token: &str) -> proto::Writer {
    let mut w = proto::Writer::new();
    w.write_string(1, "devin-cli");
    w.write_string(2, DEVIN_CLI_VERSION);
    w.write_string(3, session_token);
    w.write_string(4, "en");
    w.write_string(5, std::env::consts::OS);
    w.write_string(7, DEVIN_CLI_VERSION);
    w.write_string(12, DEVIN_PRODUCT);
    w.write_string(28, DEVIN_PRODUCT);
    w
}

/// Encode the sampling config submessage (field 8).
pub fn encode_sampling(req: &CompletionRequest, cfg: &ProviderConfig) -> proto::Writer {
    let mut w = proto::Writer::new();
    w.write_varint(1, 1);
    // The official client always sends the whole sampling block; the server rejects a
    // request that carries only part of it. Values match what it sends by default.
    w.write_varint(2, req.max_output_tokens.map_or(128_000, u64::from));
    w.write_varint(3, 400);
    let extra = cfg.extra_body.as_ref();
    let temperature = extra
        .and_then(|e| e.get("temperature"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let top_k = extra
        .and_then(|e| e.get("top_k"))
        .and_then(|v| v.as_u64())
        .unwrap_or(40);
    let top_p = extra
        .and_then(|e| e.get("top_p"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.96);
    w.write_double(5, temperature);
    w.write_varint(7, top_k);
    w.write_double(8, top_p);
    w
}

/// Encode a tool definition into protobuf (field 10).
pub fn encode_tool(tool: &ToolDefinition) -> proto::Writer {
    let mut w = proto::Writer::new();
    w.write_string(1, &tool.name);
    w.write_string(2, &tool.description);
    let schema = serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".to_string());
    w.write_string(3, &schema);
    w
}

/// Encode a pending assistant message (field 15).
pub fn encode_pending() -> proto::Writer {
    let mut w = proto::Writer::new();
    w.write_string(1, &random_uuid());
    // Field 3 is a varint on the wire: writing an empty string here made the server
    // reject the whole request with invalid_argument.
    w.write_varint(2, 1);
    w.write_varint(3, 4);
    w
}

/// Encode messages from the conversation history into protobuf messages.
// Field 10 { 1 base64_data, 2 mime_type } exists for image attachments in live traffic; unused because ContentBlock has no image variant.
pub fn encode_messages(messages: &[Message]) -> Vec<proto::Writer> {
    let mut encoded = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User | Role::System => {
                let mut w = proto::Writer::new();
                w.write_string(1, &random_uuid());
                w.write_varint(2, 1);
                w.write_string(3, &msg.text());
                encoded.push(w);
            }
            Role::Assistant => {
                let mut w = proto::Writer::new();
                w.write_string(1, &random_uuid());
                w.write_varint(2, 2);
                let text = msg.text();
                if !text.is_empty() {
                    w.write_string(3, &text);
                }
                for block in &msg.content {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        let mut tc = proto::Writer::new();
                        tc.write_string(1, id.as_str());
                        tc.write_string(2, name);
                        tc.write_string(3, &input.to_string());
                        w.write_message(6, &tc);
                    }
                }
                for block in &msg.content {
                    if let ContentBlock::Reasoning { text, signature } = block {
                        w.write_string(11, text);
                        if let Some(sig) = signature {
                            w.write_string(12, sig);
                            // The signature kind is not kept on the content block; every
                            // signature observed in live traffic was "sealed".
                            w.write_string(18, "sealed");
                        }
                    }
                }
                encoded.push(w);
            }
            Role::Tool => {
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        call_id, content, ..
                    } = block
                    {
                        let mut w = proto::Writer::new();
                        w.write_string(1, &random_uuid());
                        w.write_varint(2, 4);
                        w.write_string(3, content);
                        w.write_string(7, call_id.as_str());
                        encoded.push(w);
                    }
                }
            }
        }
    }

    encoded
}

/// Encode `GetChatMessageRequest` into protobuf bytes.
pub fn encode_get_chat_message_request(
    session_token: &str,
    req: &CompletionRequest,
    cfg: &ProviderConfig,
    assignment_jwt: Option<&str>,
) -> Vec<u8> {
    let mut w = proto::Writer::new();

    // 1: metadata
    let metadata = encode_metadata(session_token);
    w.write_message(1, &metadata);

    // 2: system_prompt
    let system_prompt = if req.system_dynamic.is_empty() {
        req.system_static.clone()
    } else if req.system_static.is_empty() {
        req.system_dynamic.clone()
    } else {
        format!("{}\n\n{}", req.system_static, req.system_dynamic)
    };
    if !system_prompt.is_empty() {
        w.write_string(2, &system_prompt);
    }

    // 3: repeated message
    for msg in encode_messages(&req.messages) {
        w.write_message(3, &msg);
    }

    // 7: varint 5
    w.write_varint(7, 5);

    // 8: sampling
    let sampling = encode_sampling(req, cfg);
    w.write_message(8, &sampling);

    // 10: repeated tool
    for tool in &req.tools {
        let t_msg = encode_tool(tool);
        w.write_message(10, &t_msg);
    }

    // 15: pending
    let pending = encode_pending();
    w.write_message(15, &pending);

    // 16: conversation id, stable for the whole conversation
    w.write_string(16, &conversation_id(req));

    // 20: varint 1
    w.write_varint(20, 1);

    // 21: model_uid
    w.write_string(21, &req.model);

    // 26: assignment_jwt
    if let Some(jwt) = assignment_jwt
        && !jwt.is_empty()
    {
        w.write_string(26, jwt);
    }

    w.into_bytes()
}

/// Encode `AssignModelRequest` into protobuf bytes.
pub fn encode_assign_model_request(
    session_token: &str,
    model_uid: &str,
    conversation_id: &str,
    latest_user_message: Option<&Message>,
) -> Vec<u8> {
    let mut w = proto::Writer::new();
    let metadata = encode_metadata(session_token);
    w.write_message(1, &metadata);
    w.write_string(2, model_uid);
    w.write_string(3, conversation_id);
    if let Some(user_msg) = latest_user_message {
        let mut msg_w = proto::Writer::new();
        msg_w.write_varint(2, 1);
        let text = user_msg.text();
        msg_w.write_string(3, &text);
        w.write_message(5, &msg_w);
    }
    w.into_bytes()
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ModelAssignment {
    pub assignment_jwt: String,
    pub assigned_model_uid: String,
    pub harness_uids: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct AssignModelResponse {
    pub assignment: Option<ModelAssignment>,
}

/// Decode an `AssignModelResponse` protobuf payload.
pub fn decode_assign_model_response(bytes: &[u8]) -> Result<AssignModelResponse, ProtoError> {
    let reader = proto::Reader::new(bytes);
    let mut resp = AssignModelResponse::default();

    for res in reader {
        let (field_no, wire_type, value) = res?;
        if field_no == 1 && wire_type == WireType::LengthDelimited {
            let a_reader = value.as_message()?;
            let mut assignment = ModelAssignment::default();
            for a_res in a_reader {
                let (af, _, av) = a_res?;
                match af {
                    1 => assignment.assignment_jwt = av.as_str()?.to_string(),
                    2 => assignment.assigned_model_uid = av.as_str()?.to_string(),
                    3 => assignment.harness_uids.push(av.as_str()?.to_string()),
                    _ => {}
                }
            }
            resp.assignment = Some(assignment);
        }
    }

    Ok(resp)
}

/// Encode `GetCliModelConfigsRequest` into protobuf bytes.
pub fn encode_get_cli_model_configs_request(session_token: &str) -> Vec<u8> {
    let mut w = proto::Writer::new();
    let metadata = encode_metadata(session_token);
    w.write_message(1, &metadata);
    w.into_bytes()
}

// ---------------------------------------------------------------------------
// Response Decoders
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChatUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

/// A tool call the server sends as a structured field rather than as inline text.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ChatToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ChatMessageDelta {
    pub message_id: Option<String>,
    pub tool_call: Option<ChatToolCall>,
    pub delta_text: Option<String>,
    pub delta_tokens: Option<u64>,
    pub stop_reason: Option<u64>,
    pub delta_thinking: Option<String>,
    pub thinking_signature: Option<String>,
    pub thinking_kind: Option<String>,
    pub provider_message_id: Option<String>,
    pub usage: Option<ChatUsage>,
    pub model_name: Option<String>,
}

/// Decode a `GetChatMessageResponse` protobuf payload.
pub fn decode_get_chat_message_response(bytes: &[u8]) -> Result<ChatMessageDelta, ProtoError> {
    let reader = proto::Reader::new(bytes);
    let mut resp = ChatMessageDelta::default();

    for res in reader {
        let (field_no, _wire_type, value) = res?;
        match field_no {
            1 => resp.message_id = Some(value.as_str()?.to_string()),
            3 => resp.delta_text = Some(value.as_str()?.to_string()),
            4 => resp.delta_tokens = Some(value.as_varint()?),
            5 => resp.stop_reason = Some(value.as_varint()?),
            6 => {
                // The server may deliver a tool call structurally here instead of
                // streaming it inside `delta_text`; both encodings occur in practice.
                let call_reader = value.as_message()?;
                let mut call = ChatToolCall::default();
                for call_res in call_reader {
                    let (cf, _, cv) = call_res?;
                    match cf {
                        1 => call.call_id = cv.as_str()?.to_string(),
                        2 => call.name = cv.as_str()?.to_string(),
                        3 => call.arguments_json = cv.as_str()?.to_string(),
                        _ => {}
                    }
                }
                if !call.name.is_empty() {
                    resp.tool_call = Some(call);
                }
            }
            7 => {
                let meta_reader = value.as_message()?;
                let mut usage = ChatUsage::default();
                for meta_res in meta_reader {
                    let (mf, _, mv) = meta_res?;
                    match mf {
                        2 => usage.input_tokens = mv.as_varint()?,
                        3 => usage.output_tokens = mv.as_varint()?,
                        5 => usage.cached_input_tokens = mv.as_varint()?,
                        9 => resp.model_name = Some(mv.as_str()?.to_string()),
                        _ => {}
                    }
                }
                resp.usage = Some(usage);
            }
            9 => resp.delta_thinking = Some(value.as_str()?.to_string()),
            10 => resp.thinking_signature = Some(value.as_str()?.to_string()),
            15 => resp.provider_message_id = Some(value.as_str()?.to_string()),
            21 => resp.thinking_kind = Some(value.as_str()?.to_string()),
            _ => {}
        }
    }

    Ok(resp)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CliModelConfig {
    pub display_name: Option<String>,
    pub credit_cost: Option<f32>,
    pub context_window: Option<u64>,
    pub model_uid: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct GetCliModelConfigsResponse {
    pub models: Vec<CliModelConfig>,
}

/// Decode a `GetCliModelConfigsResponse` protobuf payload.
pub fn decode_get_cli_model_configs_response(
    bytes: &[u8],
) -> Result<GetCliModelConfigsResponse, ProtoError> {
    let reader = proto::Reader::new(bytes);
    let mut resp = GetCliModelConfigsResponse::default();

    for res in reader {
        let (field_no, wire_type, value) = res?;
        if field_no == 1 && wire_type == WireType::LengthDelimited {
            let m_reader = value.as_message()?;
            let mut model = CliModelConfig::default();
            for m_res in m_reader {
                let (mf, _, mv) = m_res?;
                match mf {
                    1 => model.display_name = Some(mv.as_str()?.to_string()),
                    3 => model.credit_cost = Some(mv.as_float()?),
                    18 => model.context_window = Some(mv.as_varint()?),
                    22 => model.model_uid = Some(mv.as_str()?.to_string()),
                    _ => {}
                }
            }
            resp.models.push(model);
        }
    }

    Ok(resp)
}
