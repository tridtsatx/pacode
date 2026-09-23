//! Stream consumption for the turn loop: delta coalescing, live transcript updates.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use pacode_provider::EventStream;
use pacode_types::{
    CallId, ContentBlock, Event, Message, Role, StreamEvent, ToolStatus, TranscriptItem,
    TranscriptKind, TurnStop, Usage, truncate_head_tail,
};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;
use crate::transcript::DeltaCoalescer;

/// Raw argument bytes kept per in-flight call — a provider emitting unbounded
/// args must not grow memory without bound. Past it the buffer stops growing
/// and the cell preview carries a truncation note.
const RAW_ARGS_CAP: usize = 1024 * 1024;
/// The streamed-args cell preview shows head+tail only; the parsed tool
/// output replaces it when the call lands.
const ARGS_PREVIEW_CAP: usize = 6_000;

/// A tool call still streaming: raw args accumulate under a cap while the
/// transcript cell created at `ToolCallStart` shows them live.
struct BuildingToolCall {
    call_id: CallId,
    name: String,
    /// Raw JSON args received so far (bounded by `RAW_ARGS_CAP`).
    args: String,
    /// Set once the cap was hit — the cell notes the truncation exactly once.
    args_capped: bool,
    /// The transcript cell allocated at `ToolCallStart`; reused by dispatch.
    item_seq: u64,
    /// Coalesces `ItemUpdated` emissions for the args preview.
    coalescer: DeltaCoalescer,
}

/// A fully received tool call handed to dispatch.
pub struct PendingToolCall {
    pub call_id: CallId,
    pub name: String,
    pub input: serde_json::Value,
    pub parse_err: Option<String>,
    /// Transcript cell seq created at `ToolCallStart`; `None` only when the
    /// call arrived without a start event — dispatch creates the cell then.
    pub item_seq: Option<u64>,
}

pub struct StreamOutcome {
    pub text: String,
    pub reasoning: String,
    pub reasoning_signature: Option<String>,
    pub tool_calls: Vec<PendingToolCall>,
    pub usage: Usage,
    pub interrupted: bool,
}

/// The transcript cell for a streaming call: `ItemUpdated` with the raw args
/// received so far as the preview (display-capped). Title/intent stay
/// provisional until dispatch parses the args.
fn emit_tool_call_update(session: &Arc<Session>, agent: &Arc<Agent>, building: &BuildingToolCall) {
    let mut preview = truncate_head_tail(&building.args, ARGS_PREVIEW_CAP);
    if building.args_capped {
        preview.push_str("\n…[arguments truncated at 1 MiB]");
    }
    let item = TranscriptItem {
        seq: building.item_seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        kind: TranscriptKind::ToolCall {
            call_id: building.call_id.clone(),
            name: building.name.clone(),
            title: building.name.clone(),
            intent: None,
            status: ToolStatus::Running,
            preview,
            diff: None,
            duration_ms: None,
            task: None,
        },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemUpdated(item));
}

/// The stream died (interrupt or provider failure) before these calls could
/// dispatch: their cells must not stay `Running` forever.
fn fail_streaming_tool_calls(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    tool_calls_building: &BTreeMap<u32, BuildingToolCall>,
) {
    for building in tool_calls_building.values() {
        let item = TranscriptItem {
            seq: building.item_seq,
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::ToolCall {
                call_id: building.call_id.clone(),
                name: building.name.clone(),
                title: building.name.clone(),
                intent: None,
                status: ToolStatus::Error,
                preview: "cancelled".to_string(),
                diff: None,
                duration_ms: None,
                task: None,
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemUpdated(item));
    }
}

pub async fn consume_stream(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    mut stream: EventStream,
) -> Result<StreamOutcome, TurnStop> {
    let mut text_coalescer = DeltaCoalescer::new();
    let mut reasoning_coalescer = DeltaCoalescer::new();
    let mut assistant_item_seq: Option<u64> = None;
    let mut reasoning_item_seq: Option<u64> = None;
    let mut text_acc = String::new();
    let mut reasoning_acc = String::new();
    let mut reasoning_signature: Option<String> = None;
    let mut tool_calls_building: BTreeMap<u32, BuildingToolCall> = BTreeMap::new();
    // call_id → cell seq, for provider retries that re-issue `ToolCallStart`.
    let mut tool_call_seqs: HashMap<CallId, u64> = HashMap::new();
    let mut turn_usage = Usage::default();

    let stream_interrupted = loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break true;
            }
            res = tokio::time::timeout(Duration::from_millis(25), stream.next()) => {
                let now = Instant::now();
                match res {
                    Err(_timeout) => {
                        if let Some(flush) = text_coalescer.take_if_due(now)
                            && let Some(seq) = assistant_item_seq
                        {
                            session.events.emit(Event::TextDelta {
                                agent: agent.id(),
                                item_seq: seq,
                                text: flush,
                            });
                        }
                        if let Some(flush) = reasoning_coalescer.take_if_due(now)
                            && let Some(seq) = reasoning_item_seq
                        {
                            session.events.emit(Event::ReasoningDelta {
                                agent: agent.id(),
                                item_seq: seq,
                                text: flush,
                            });
                        }
                        for building in tool_calls_building.values_mut() {
                            if building.coalescer.take_if_due(now).is_some() {
                                emit_tool_call_update(session, agent, building);
                            }
                        }
                    }
                    Ok(None) => {
                        break false;
                    }
                    Ok(Some(event_res)) => {
                        match event_res {
                            Ok(StreamEvent::TextDelta { text }) => {
                                if assistant_item_seq.is_none() {
                                    let item_seq = agent
                                        .transcript
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .next_seq();
                                    assistant_item_seq = Some(item_seq);
                                    let item = TranscriptItem {
                                        seq: item_seq,
                                        agent: agent.id(),
                                        ts_ms: pacode_types::now_ms(),
                                        kind: TranscriptKind::Assistant {
                                            text: String::new(),
                                            complete: false,
                                        },
                                    };
                                    agent
                                        .transcript
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .upsert(item.clone());
                                    session.events.emit(Event::ItemAdded(item));
                                }
                                text_acc.push_str(&text);
                                if let Some(flush) = text_coalescer.push(&text, now)
                                    && let Some(seq) = assistant_item_seq
                                {
                                    session.events.emit(Event::TextDelta {
                                        agent: agent.id(),
                                        item_seq: seq,
                                        text: flush,
                                    });
                                }
                            }
                            Ok(StreamEvent::ReasoningDelta { text }) => {
                                if reasoning_item_seq.is_none() {
                                    let item_seq = agent
                                        .transcript
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .next_seq();
                                    reasoning_item_seq = Some(item_seq);
                                    let item = TranscriptItem {
                                        seq: item_seq,
                                        agent: agent.id(),
                                        ts_ms: pacode_types::now_ms(),
                                        kind: TranscriptKind::Reasoning {
                                            text: String::new(),
                                            complete: false,
                                        },
                                    };
                                    agent
                                        .transcript
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .upsert(item.clone());
                                    session.events.emit(Event::ItemAdded(item));
                                }
                                reasoning_acc.push_str(&text);
                                if let Some(flush) = reasoning_coalescer.push(&text, now)
                                    && let Some(seq) = reasoning_item_seq
                                {
                                    session.events.emit(Event::ReasoningDelta {
                                        agent: agent.id(),
                                        item_seq: seq,
                                        text: flush,
                                    });
                                }
                            }
                            Ok(StreamEvent::ReasoningSignature { signature, kind: _ }) => {
                                reasoning_signature = Some(signature);
                            }
                            Ok(StreamEvent::ToolCallStart { index, id, name }) => {
                                // Create the transcript cell immediately so the
                                // call is visible while its arguments stream in.
                                // A provider-internal retry can re-issue the same
                                // call id (usually at the same index): reuse that
                                // cell rather than stacking a duplicate.
                                let item_seq = match tool_call_seqs.get(&id) {
                                    Some(seq) => *seq,
                                    None => {
                                        let seq = agent
                                            .transcript
                                            .lock()
                                            .unwrap_or_else(|p| p.into_inner())
                                            .next_seq();
                                        tool_call_seqs.insert(id.clone(), seq);
                                        let item = TranscriptItem {
                                            seq,
                                            agent: agent.id(),
                                            ts_ms: pacode_types::now_ms(),
                                            kind: TranscriptKind::ToolCall {
                                                call_id: id.clone(),
                                                name: name.clone(),
                                                title: name.clone(),
                                                intent: None,
                                                status: ToolStatus::Running,
                                                preview: String::new(),
                                                diff: None,
                                                duration_ms: None,
                                                task: None,
                                            },
                                        };
                                        agent
                                            .transcript
                                            .lock()
                                            .unwrap_or_else(|p| p.into_inner())
                                            .upsert(item.clone());
                                        session.events.emit(Event::ItemAdded(item));
                                        seq
                                    }
                                };
                                // A retry may re-issue the same call under a
                                // different index; drop the stale attempt so the
                                // call dispatches once.
                                tool_calls_building.retain(|_, b| b.call_id != id);
                                tool_calls_building.insert(
                                    index,
                                    BuildingToolCall {
                                        call_id: id,
                                        name,
                                        args: String::new(),
                                        args_capped: false,
                                        item_seq,
                                        coalescer: DeltaCoalescer::new(),
                                    },
                                );
                            }
                            Ok(StreamEvent::ToolCallArgsDelta { index, delta }) => {
                                if let Some(building) = tool_calls_building.get_mut(&index) {
                                    if building.args.len() + delta.len() > RAW_ARGS_CAP {
                                        // Past the cap: keep the buffer, note the
                                        // truncation once, stop feeding the
                                        // coalescer — no more args updates.
                                        if !building.args_capped {
                                            building.args_capped = true;
                                            emit_tool_call_update(session, agent, building);
                                        }
                                    } else {
                                        building.args.push_str(&delta);
                                        if building.coalescer.push(&delta, now).is_some() {
                                            emit_tool_call_update(session, agent, building);
                                        }
                                    }
                                }
                            }
                            Ok(StreamEvent::MessageStart { .. }) => {}
                            Ok(StreamEvent::Usage(u)) => {
                                turn_usage = u;
                            }
                            Ok(StreamEvent::MessageEnd { .. }) => {}
                            Err(e) => {
                                log::warn!("stream error during turn: {e}");
                                fail_streaming_tool_calls(session, agent, &tool_calls_building);
                                return Err(TurnStop::Failed {
                                    message: e.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
    };

    if let Some(flush) = text_coalescer.take()
        && let Some(seq) = assistant_item_seq
    {
        session.events.emit(Event::TextDelta {
            agent: agent.id(),
            item_seq: seq,
            text: flush,
        });
    }
    if let Some(flush) = reasoning_coalescer.take()
        && let Some(seq) = reasoning_item_seq
    {
        session.events.emit(Event::ReasoningDelta {
            agent: agent.id(),
            item_seq: seq,
            text: flush,
        });
    }

    if let Some(seq) = reasoning_item_seq {
        let item = TranscriptItem {
            seq,
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::Reasoning {
                text: reasoning_acc.clone(),
                complete: true,
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemUpdated(item));
    }

    if let Some(seq) = assistant_item_seq {
        let item = TranscriptItem {
            seq,
            agent: agent.id(),
            ts_ms: pacode_types::now_ms(),
            kind: TranscriptKind::Assistant {
                text: text_acc.clone(),
                complete: true,
            },
        };
        agent
            .transcript
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(item.clone());
        session.events.emit(Event::ItemUpdated(item));
    }

    if stream_interrupted {
        fail_streaming_tool_calls(session, agent, &tool_calls_building);
        if !text_acc.is_empty() || !reasoning_acc.is_empty() || reasoning_signature.is_some() {
            let mut blocks = Vec::new();
            if !reasoning_acc.is_empty() || reasoning_signature.is_some() {
                blocks.push(ContentBlock::Reasoning {
                    text: reasoning_acc.clone(),
                    signature: reasoning_signature.clone(),
                });
            }
            if !text_acc.is_empty() {
                blocks.push(ContentBlock::Text {
                    text: text_acc.clone(),
                });
            }
            let msg = Message::new(Role::Assistant, blocks);
            let (seq, arc_msg) = agent
                .history
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(msg);
            let _ = session
                .store
                .append_message(&session.id, &agent.id(), seq, &arc_msg)
                .await;
        }
        return Ok(StreamOutcome {
            text: text_acc,
            reasoning: reasoning_acc,
            reasoning_signature,
            tool_calls: Vec::new(),
            usage: turn_usage,
            interrupted: true,
        });
    }

    let mut tool_calls = Vec::new();
    for (_, building) in tool_calls_building {
        let (parse_err, input_val) = match serde_json::from_str::<serde_json::Value>(&building.args)
        {
            Ok(v) => (None, v),
            Err(e) => (Some(e.to_string()), serde_json::json!({})),
        };
        tool_calls.push(PendingToolCall {
            call_id: building.call_id,
            name: building.name,
            input: input_val,
            parse_err,
            item_seq: Some(building.item_seq),
        });
    }

    Ok(StreamOutcome {
        text: text_acc,
        reasoning: reasoning_acc,
        reasoning_signature,
        tool_calls,
        usage: turn_usage,
        interrupted: false,
    })
}
