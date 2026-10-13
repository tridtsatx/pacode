//! What a subagent keeps across a reopen of its session: who stopped it, and
//! the tools it was spawned with.

use std::sync::Arc;

use pacode_tools::{AgentSpec, SendOutcome};
use pacode_types::{AgentId, Attach};
use tokio::sync::Semaphore;

use super::StopOrigin;
use super::harness::*;
use crate::agent::Agent;
use crate::session::Session;

fn spec(name: &str, tools: Option<&[&str]>, exclude: Option<&[&str]>) -> AgentSpec {
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_string()).collect();
    AgentSpec {
        prompt: "work".into(),
        name: Some(name.into()),
        model: None,
        effort: None,
        fork: false,
        tools: tools.map(names),
        exclude_tools: exclude.map(names),
    }
}

/// Close the session and open it again from the store.
async fn reopen(h: &Harness) -> Arc<Session> {
    let id = h.session.id.clone();
    h.core.close_session(&id).await;
    h.core
        .open_session(Attach::Resume {
            session: id.clone(),
        })
        .await
        .unwrap();
    h.core.session(&id).unwrap()
}

fn tool_names(agent: &Agent) -> Vec<String> {
    let mut names: Vec<String> = agent
        .tools
        .read()
        .unwrap()
        .names()
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

/// A daemon restart does not turn a subagent the user stopped into one the
/// orchestrator may resume — and does not make one the orchestrator stopped,
/// or the session close stopped, the user's.
#[tokio::test(flavor = "multi_thread")]
async fn who_stopped_a_subagent_survives_a_reopen() {
    let h = Harness::new().await;
    let gate = Arc::new(Semaphore::new(0));
    h.provider
        .script(true, vec![gated(&gate, text("a")), gated(&gate, text("b"))]);
    let by_user = h
        .session
        .spawn_agent(spec("by-user", None, None))
        .await
        .unwrap();
    let by_orchestrator = h
        .session
        .spawn_agent(spec("by-orchestrator", None, None))
        .await
        .unwrap();
    wait_for("both in their first step", || {
        h.provider.seen(true).len() == 2
    })
    .await;
    h.session.stop_agent(&by_user).await.unwrap();
    h.session
        .stop_agent_by(&by_orchestrator, StopOrigin::Orchestrator)
        .await
        .unwrap();
    for id in [&by_user, &by_orchestrator] {
        let agent = h.session.agent(id).unwrap();
        wait_for("the stop", || !agent.is_running()).await;
    }

    let session = reopen(&h).await;
    let main = session.main_agent().unwrap();
    let err = super::send(&session, &main, &by_user, "go on", super::Consent::Absent)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");
    let host = crate::host::SessionHost {
        session: session.clone(),
        agent: AgentId::main(),
    };
    let err = pacode_tools::ToolHost::continue_agent(&host, &by_user, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stopped by the user"), "{err}");

    h.provider.script(true, vec![text("resumed")]);
    let outcome = super::send(
        &session,
        &main,
        &by_orchestrator,
        "go on",
        super::Consent::Absent,
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        SendOutcome::Resumed {
            previous_report: None
        }
    );
    gate.add_permits(2);
    h.core.shutdown().await;
}

/// A subagent spawned with a narrowed tool set — a `tools` list and an
/// `exclude_tools` that drops one of the subagent-only tools — comes back
/// from a reopen with that set, not the default one.
#[tokio::test(flavor = "multi_thread")]
async fn the_tools_a_subagent_was_spawned_with_survive_a_reopen() {
    let h = Harness::new().await;
    h.provider.script(true, vec![text("done")]);
    let id = h
        .session
        .spawn_agent(spec("narrow", Some(&["gate"]), Some(&["ask_parent"])))
        .await
        .unwrap();
    let sub = h.session.agent(&id).unwrap();
    assert_eq!(tool_names(&sub), ["gate", "report_status"]);
    wait_for("the subagent to finish", || !sub.is_running()).await;

    let session = reopen(&h).await;
    let sub = session.agent(&id).unwrap();
    assert_eq!(tool_names(&sub), ["gate", "report_status"]);
    h.core.shutdown().await;
}
