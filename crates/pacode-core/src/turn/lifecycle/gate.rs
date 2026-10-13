//! Test-only gate that parks a turn's supervisor at fixed points of its
//! post-block, so a test can land an injector in exactly that window.

use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use tokio::sync::Semaphore;

use crate::agent::Agent;
use crate::session::Session;

pub(crate) struct LifecycleGate {
    expected_agent: StdMutex<Option<String>>,
    pub(crate) reached: Semaphore,
    pub(crate) release: Semaphore,
    /// One-shot. A turn now passes two gate points (subagent finalization and
    /// the window before the lock is released), and a gate that fired at both
    /// would hand the test two arrivals to tell apart — the earlier one
    /// arriving after it had already released the second.
    fired: std::sync::atomic::AtomicBool,
}

static LIFECYCLE_GATE: OnceLock<StdMutex<Option<Arc<LifecycleGate>>>> = OnceLock::new();

pub(super) fn install_lifecycle_gate(session: &Session, agent: &Agent) -> Arc<LifecycleGate> {
    let gate = Arc::new(LifecycleGate {
        expected_agent: StdMutex::new(Some(format!(
            "{}:{}",
            session.id.as_str(),
            agent.id().as_str()
        ))),
        reached: Semaphore::new(0),
        release: Semaphore::new(0),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    *LIFECYCLE_GATE
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(gate.clone());
    gate
}

pub(super) async fn wait_for_lifecycle_gate(session: &Session, agent: &Agent) {
    let Some(gate) = LIFECYCLE_GATE.get().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }) else {
        return;
    };
    let expected = format!("{}:{}", session.id.as_str(), agent.id().as_str());
    let matches = gate
        .expected_agent
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_deref()
        .is_some_and(|expected_owner| expected_owner == expected);
    // One-shot: a turn passes two gate points now, and firing at both would
    // leave a test holding two arrivals it cannot tell apart.
    if matches && !gate.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
        gate.reached.add_permits(1);
        let _ = gate.release.acquire().await;
    }
}
