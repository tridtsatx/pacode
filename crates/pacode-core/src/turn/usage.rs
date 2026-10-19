//! Persist one finished turn's usage (perf C7). Owner: turn loop; fire-and-forget.

use pacode_types::Usage;

use crate::agent::Agent;
use crate::session::Session;

/// Append the turn's usage to the store's `usage` table so `usage_totals` survives a
/// restart. Call after `Session::record_usage` (the turn number is the session's turn
/// counter, so it is unique and increasing). A write failure is only logged.
pub(super) fn persist(session: &Session, agent: &Agent, usage: Usage) {
    let turn_no = session
        .usage
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .turns;
    let model = session.meta().model.model;
    let cost = session
        .config
        .pricing_for(&model)
        .map(|p| p.cost_usd(&usage));
    let store = session.store.clone();
    let sid = session.id.clone();
    let aid = agent.id();
    tokio::spawn(async move {
        if let Err(e) = store.add_usage(&sid, &aid, turn_no, &usage, cost).await {
            log::warn!("persist usage failed: session={sid} agent={aid} turn={turn_no}: {e}");
        }
    });
}
