//! The turn loop (spec §6.2).
//!
//! ```text
//! loop {
//!   build CompletionRequest (system_static, system_dynamic, tools, history.for_model())
//!   stream = provider.complete(req)            // retries inside the provider
//!   consume: TextDelta → coalesce → Event::TextDelta; ReasoningDelta likewise;
//!            ToolCallStart/ArgsDelta → assemble ToolUse blocks; Usage → record;
//!            MessageEnd → stop reason. Cancel token → abort stream, TurnStop::Interrupted.
//!   history.push(assistant message)  (persist)  // append-only
//!   emit ItemUpdated(assistant complete)
//!   if no tool calls {
//!     // point B
//!     match injections.drain() { empty → break; items → push render_injections; continue }
//!   }
//!   run tool calls: ReadOnly/Network in parallel (join_all), others sequentially, each:
//!     gate → maybe ask permission (WaitingApproval) → call → cap output → history.push(tool_result) (persist)
//!     emit ItemAdded/ItemUpdated(ToolCall …); cancel → stub tool results ("cancelled") then Interrupted
//!   // point C: urgent interrupt only (cancel token)
//!   // point D
//!   history.extend(render_injections(injections.drain()))
//!   if needs_compaction → compaction::compact
//!   subagent: turns += 1; stop at agents.max_turns with a notice
//! }
//! ```

use std::sync::Arc;

use codeapp_types::TurnStop;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;

/// Run one turn of `agent` inside `session`. Sets agent status and emits
/// `TurnStarted`/`TurnEnded`. Never panics; every error becomes `TurnStop::Failed`.
pub async fn run_turn(
    session: Arc<Session>,
    agent: Arc<Agent>,
    cancel: CancellationToken,
) -> TurnStop {
    let _ = (session, agent, cancel);
    todo!("turn::run_turn")
}

/// Spawn `run_turn` as a tokio task, storing the cancel token on the agent. Returns
/// immediately. When the turn ends, the token is cleared and, for subagents, the
/// parent gets an `Injection::AgentFinished` and the agent's history is persisted
/// and dropped from memory (info + summary stay).
pub fn start_turn(session: Arc<Session>, agent: Arc<Agent>) {
    let _ = (session, agent);
    todo!("turn::start_turn")
}
