//! Fusion: a two-model pipeline for one turn (spec §6.2, `[ui] fusion`).
//!
//! The planner is the expensive model and thinks; the implementer is the cheap
//! model and writes. One user turn becomes:
//!
//! ```text
//! user turn
//!   └─ planner turn   — read-only tool set, scratch history, produces a plan
//!        └─ plan      — committed to the transcript as its own step
//!             └─ implementer turn — the agent's normal loop, on the plan
//! ```
//!
//! Three decisions are load-bearing and are not negotiable further down:
//!
//! * **The split is enforced in the request, not in the model's manners.** The
//!   planner's request carries `planner::ALLOWED_TOOLS` and nothing else; a
//!   write call that arrives anyway is refused with an explicit tool result
//!   (`planner::refusal_text`) and the planner turn continues.
//! * **The plan is a persisted artifact.** It lands in history as one user-role
//!   message and in the transcript as the item that message rebuilds to, so a
//!   resumed session shows exactly what the live one showed. A fusion turn with
//!   no trace in the transcript is a turn that cannot be resumed or audited.
//! * **Failure is asymmetric on purpose.** A planner that fails, refuses, times
//!   out or returns nothing leaves history untouched and the turn degrades to
//!   the primary model doing the whole job — the user sees one ordinary turn. An
//!   implementer that fails keeps the plan (the expensive call was real and
//!   paid for) and reports the failure.
//!
//! Where it runs: **main-agent turns only.** A subagent turn is already scoped
//! and budgeted, and doubling its calls buys nothing; compaction is a summary
//! call, not a turn, and never enters this path. A turn that only asks a
//! question *does* run: the planner's read pass is the reading, and the cheap
//! model answers from the plan instead of re-reading the repository itself.

mod planner;

use std::sync::Arc;

use pacode_types::{Event, Message, ModelRoute, TranscriptItem, TranscriptKind};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::session::Session;

/// Chars of plan kept for the implementer. The plan lives in history for the
/// rest of the session, so it is bounded like any other injected text; past the
/// cap the head+tail cut keeps the shape of an answer at both ends.
pub const PLAN_CAP_CHARS: usize = 16_000;

/// The two routes a fusion turn runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FusionRoutes {
    pub planner: ModelRoute,
    pub implementer: ModelRoute,
}

/// What produced the plan, and what it cost. Recorded before the plan is
/// committed so a failure between the two still bills the real call.
#[derive(Clone, Debug)]
pub(crate) struct PlanArtifact {
    pub route: ModelRoute,
    pub text: String,
    pub usage: pacode_types::Usage,
}

/// What came out of the planner half. Every variant that is not a plan
/// degrades to the single-model path — and still carries the spend, because a
/// planner that read four files and then gave up cost the user four reads.
#[derive(Clone, Debug)]
pub(crate) enum PlanOutcome {
    Planned(PlanArtifact),
    Skipped {
        /// For the log and the trace, not for the user: a degraded turn shows
        /// one ordinary conversation, and a notice about the failed planner
        /// would be a second thing the user has to read.
        reason: String,
        usage: pacode_types::Usage,
    },
}

impl PlanOutcome {
    /// A planner that never reached the provider.
    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        PlanOutcome::Skipped {
            reason: reason.into(),
            usage: pacode_types::Usage::default(),
        }
    }
}

/// Where a turn is in the pipeline. Built once per turn and threaded through
/// the turn loop, so the request builder can ask "which model" in one place.
#[derive(Clone, Debug)]
pub(crate) enum Phase {
    /// Not configured for this turn: the single-model path, byte for byte.
    Off,
    /// Configured; the planner has not run yet.
    Pending(FusionRoutes),
    /// The planner ran and did not produce a plan. The turn continues on the
    /// agent's own model; `Pending` is never re-entered, so a failing planner
    /// costs one call per turn, not one per step.
    Degraded,
    /// A plan is committed: the loop runs on the implementer route.
    Planned(FusionRoutes),
}

impl Phase {
    /// The route this turn's next request goes to. `Off`/`Degraded` resolve to
    /// the agent's own model, which is exactly the pre-fusion behaviour.
    pub(crate) fn model(&self, agent_model: &ModelRoute) -> ModelRoute {
        match self {
            Phase::Planned(routes) => routes.implementer.clone(),
            Phase::Off | Phase::Pending(_) | Phase::Degraded => agent_model.clone(),
        }
    }

    /// The routes to run the planner with, once, before the first request.
    pub(crate) fn take_pending(&mut self) -> Option<FusionRoutes> {
        match std::mem::replace(self, Phase::Degraded) {
            Phase::Pending(routes) => Some(routes),
            other => {
                *self = other;
                None
            }
        }
    }

    pub(crate) fn planned(&self) -> bool {
        matches!(self, Phase::Planned(_))
    }
}

/// What the user is told when a fusion turn's provider call fails.
///
/// Names the half that failed and the route it was on, because a two-model turn
/// that dies halfway otherwise reads as the client's own model failing for no
/// reason.
pub(crate) fn failure_message(phase: &Phase, error: &str) -> String {
    match phase {
        Phase::Planned(routes) => {
            format!(
                "fusion implementer ({}) failed: {error}",
                routes.implementer
            )
        }
        // `Degraded` has already spent the planner and runs on the agent's own
        // model, so naming the planner would blame a model that already
        // answered. `Off` was never a fusion turn.
        Phase::Degraded | Phase::Off => format!("request failed: {error}"),
        // A turn still `Pending` is failing in its planner: this is its first
        // request, and the first request is the planner's.
        Phase::Pending(routes) => {
            format!("fusion planner ({}) failed: {error}", routes.planner)
        }
    }
}

/// The configured pair, or `None` when this turn must not run a pipeline.
///
/// A pair that parses to the same route twice is refused: it would bill two
/// calls per step for one model's answer.
pub(crate) fn routes_for(session: &Session, agent: &Agent) -> Option<FusionRoutes> {
    // Main agent only — see the module doc.
    if !agent.id().is_main() {
        return None;
    }
    let config = session.config.ui.fusion.as_ref()?;
    let routes = FusionRoutes {
        planner: ModelRoute::parse_lossy(&config.planner)?,
        implementer: ModelRoute::parse_lossy(&config.implementer)?,
    };
    if routes.planner == routes.implementer {
        log::warn!(
            "fusion: planner and implementer are both {}; running one model per turn instead",
            routes.planner
        );
        return None;
    }
    Some(routes)
}

/// The initial phase for a turn.
pub(crate) fn start_phase(session: &Session, agent: &Agent) -> Phase {
    match routes_for(session, agent) {
        Some(routes) => Phase::Pending(routes),
        None => Phase::Off,
    }
}

/// The tool definitions the planner's requests carry; the caller renders the
/// planner's `system_dynamic` from them so its `## Tools` inventory matches.
pub(crate) fn planner_tool_definitions(agent: &Agent) -> Vec<pacode_types::ToolDefinition> {
    planner::registry(agent).definitions()
}

/// Run the planner half of the turn. The planner sees the very request the
/// implementer is about to see, minus the tools: `system_static`/`system_dynamic`
/// are passed in rather than rebuilt so the two halves cannot drift apart (the
/// dynamic part's `## Tools` inventory is rendered from
/// [`planner_tool_definitions`]), and the history is the agent's own, read at
/// this point in the loop.
pub(crate) async fn run_planner(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    cancel: &CancellationToken,
    routes: &FusionRoutes,
    system_static: &str,
    system_dynamic: &str,
) -> PlanOutcome {
    if cancel.is_cancelled() {
        return PlanOutcome::skipped("interrupted before the planner ran");
    }
    // ZDR is checked per provider call, not per route choice, and a planner
    // blocked here is a planner failure: the primary below is checked by the
    // turn loop itself, so degrading leaks nothing.
    if let Some(reason) =
        crate::permissions::zdr_route_violation(session, &routes.planner.to_string())
    {
        return PlanOutcome::skipped(reason);
    }
    let provider = match session.providers.resolve(&routes.planner).await {
        Ok(provider) => provider,
        Err(error) => return PlanOutcome::skipped(error.to_string()),
    };
    planner::run(
        session,
        agent,
        cancel,
        provider,
        routes,
        system_static,
        system_dynamic,
    )
    .await
}

/// Commit the plan as a turn step: one persisted user-role message plus the
/// transcript item that message rebuilds to on resume. Both halves matter —
/// the message is what the implementer reads and what `/detach` keeps, the item
/// is what the user saw when it happened.
pub(crate) async fn commit_plan(
    session: &Arc<Session>,
    agent: &Arc<Agent>,
    artifact: &PlanArtifact,
) -> Result<(), crate::CoreError> {
    let text = plan_message(&artifact.route, &artifact.text);
    crate::turn::persist_history_message(session, agent, Message::user(text.clone())).await?;

    let seq = agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .next_seq();
    let item = TranscriptItem {
        seq,
        agent: agent.id(),
        ts_ms: pacode_types::now_ms(),
        // Byte-identical to what `history_to_items` rebuilds from the message
        // above, which is what makes a resumed fusion turn replay the same.
        kind: TranscriptKind::User { text },
    };
    agent
        .transcript
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upsert(item.clone());
    session.events.emit(Event::ItemAdded(item));
    Ok(())
}

/// The stored form of a plan. The tag matches the injection convention so the
/// implementer reads it as a brief it was handed rather than as a user turn,
/// and the body is capped: this text stays in history for the rest of the
/// session.
pub(crate) fn plan_message(route: &ModelRoute, plan: &str) -> String {
    let body = pacode_types::truncate_head_tail(plan.trim(), PLAN_CAP_CHARS);
    format!(
        "<plan model=\"{route}\">\nA read-only planning pass by {route} ran before you. \
         Implement it as written; do not re-plan, and do not ask the planner to make the edits.\n{body}\n</plan>"
    )
}

/// Which half of a fusion turn is running, or how it ended. `PlannerFailed` is
/// the degraded case: the turn still happened, on one model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FusionPhase {
    Planning,
    Implementing,
    Complete,
    PlannerFailed,
}

/// Cost and attribution for one fusion turn, on the agent so a client can ask
/// what a turn cost after the fact instead of inferring it from two models'
/// worth of events. Replaced wholesale at turn start (the invalidation point)
/// and updated as the halves land; `None` whenever fusion is off.
#[derive(Clone, Debug)]
pub struct FusionTrace {
    pub planner: ModelRoute,
    pub implementer: ModelRoute,
    pub planner_usage: pacode_types::Usage,
    pub implementer_usage: pacode_types::Usage,
    pub phase: FusionPhase,
}

impl FusionTrace {
    fn begin(routes: &FusionRoutes) -> Self {
        Self {
            planner: routes.planner.clone(),
            implementer: routes.implementer.clone(),
            planner_usage: pacode_types::Usage::default(),
            implementer_usage: pacode_types::Usage::default(),
            phase: FusionPhase::Planning,
        }
    }

    fn planner_done(&mut self, usage: pacode_types::Usage) {
        self.planner_usage = usage;
        self.phase = FusionPhase::Implementing;
    }

    pub(crate) fn implementer_done(&mut self, usage: pacode_types::Usage) {
        self.implementer_usage = usage;
        self.phase = FusionPhase::Complete;
    }

    pub(crate) fn planner_failed(&mut self, usage: pacode_types::Usage) {
        self.planner_usage = usage;
        self.phase = FusionPhase::PlannerFailed;
    }
}

/// Publish the trace for a turn that is about to run its planner.
pub(crate) fn begin_trace(agent: &Agent, routes: &FusionRoutes) {
    let mut slot = agent
        .fusion
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(FusionTrace::begin(routes));
}

/// Fold the planner's real usage into the published trace. Called before the
/// plan is committed, so a commit that fails still reports what was spent.
pub(crate) fn record_planner_usage(agent: &Agent, usage: pacode_types::Usage) {
    if let Some(trace) = agent
        .fusion
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        trace.planner_done(usage);
    }
}

/// Mark a turn that reached its end on the implementer.
pub(crate) fn complete_trace(agent: &Agent, usage: pacode_types::Usage) {
    if let Some(trace) = agent
        .fusion
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        trace.implementer_done(usage);
    }
}

/// Mark a turn whose planner did not produce a plan. The turn itself carries
/// on, on the agent's own model; the trace records what the attempt cost.
pub(crate) fn mark_planner_failed(agent: &Agent, usage: pacode_types::Usage) {
    if let Some(trace) = agent
        .fusion
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        trace.planner_failed(usage);
    }
}

/// Fold the implementer's observed usage in. Called on every model step of the
/// implementer turn, so the trace holds the turn's total rather than the last
/// step's.
pub(crate) fn record_implementer_usage(agent: &Agent, usage: pacode_types::Usage) {
    if let Some(trace) = agent
        .fusion
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        trace.implementer_usage = usage;
    }
}

/// The live status line for each half. `AgentInfo::activity` is the only
/// channel a client already renders, so the phase labels ride on it rather
/// than on a new event the TUI would have to learn.
pub(crate) fn planner_activity(route: &ModelRoute) -> String {
    format!("plan · {}", route.model)
}

pub(crate) fn implementer_activity(route: &ModelRoute) -> String {
    format!("implement · {}", route.model)
}

#[cfg(test)]
#[path = "fusion_tests.rs"]
mod fusion_tests;
