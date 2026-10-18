//! Stream consumption for the turn loop: delta coalescing, live transcript updates.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use pacode_provider::EventStream;
use pacode_types::{
    CallId, ContentBlock, Event, Message, Role, StreamEvent, TranscriptItem, TranscriptKind,
    TurnStop, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;
use crate::transcript::DeltaCoalescer;

pub struct StreamOutcome {
    pub text: String,
    pub reasoning: String,
    pub reasoning_signature: Option<String>,
    pub tool_calls: Vec<(CallId, String, serde_json::Value, Option<String>)>,
    pub usage: Usage,
    pub interrupted: bool,
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
    let mut tool_calls_building: BTreeMap<u32, (CallId, String, String)> = BTreeMap::new();
    let mut turn_usage = Usage::default();
    // Style guard: `mode: stream` rules scan write/edit arguments as they arrive. On a
    // hit the stream is dropped and the offending call comes back as a tool error that
    // carries the rule (index of the call, error text).
    let mut style_guard = crate::style_guard::StreamGuard::new(session).await;
    let mut style_stop: Option<(u32, String)> = None;

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
                                tool_calls_building.insert(index, (id, name, String::new()));
                            }
                            Ok(StreamEvent::ToolCallArgsDelta { index, delta }) => {
                                if let Some((_, name, args)) =
                                    tool_calls_building.get_mut(&index)
                                {
                                    args.push_str(&delta);
                                    if let Some(guard) = style_guard.as_mut()
                                        && let Some(hit) = guard
                                            .check(session, agent, index, name.as_str(), args.as_str())
                                            .await
                                    {
                                        style_stop = Some((index, hit.message));
                                        break false;
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
    for (index, (call_id, name, raw_args)) in tool_calls_building {
        if let Some((stop_index, message)) = &style_stop
            && *stop_index == index
        {
            tool_calls.push((call_id, name, serde_json::json!({}), Some(message.clone())));
            continue;
        }
        let (parse_err, input_val) = match serde_json::from_str::<serde_json::Value>(&raw_args) {
            Ok(v) => (None, v),
            Err(e) => (Some(e.to_string()), serde_json::json!({})),
        };
        tool_calls.push((call_id, name, input_val, parse_err));
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
