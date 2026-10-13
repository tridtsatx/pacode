//! Per-turn tool call allowances: every call of a turn is charged here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use pacode_types::{AgentId, SessionId, TurnId};

use crate::agent::Agent;
use crate::session::Session;

/// Turns whose budget counters are still held, before the oldest is dropped.
/// Dropping an entry only costs that turn its counters (it restarts from zero),
/// never its lock — the map is a ledger, not a guard.
const MAX_TRACKED_TURN_BUDGETS: usize = 1024;

/// Per-turn allowances, and the two knobs they were read from. `[limits]
/// .max_tool_calls_per_turn` / `max_exec_calls_per_turn` carried these until the
/// limits table was cut down to the provider ceiling; the values below are the
/// ones those keys defaulted to, and `turn_budget_limits` is the single place to
/// read them again once the keys are back.
const DEFAULT_MAX_TOOL_CALLS_PER_TURN: u32 = 1024;
const DEFAULT_MAX_EXEC_CALLS_PER_TURN: u32 = 128;

pub(super) fn turn_budget_limits(_session: &Session) -> BudgetLimits {
    BudgetLimits {
        tool_calls: DEFAULT_MAX_TOOL_CALLS_PER_TURN,
        exec_calls: DEFAULT_MAX_EXEC_CALLS_PER_TURN,
    }
}

#[derive(Default)]
pub(super) struct TurnBudget {
    tool_calls: AtomicU32,
    exec_calls: AtomicU32,
}

#[derive(Clone, Copy)]
pub(super) struct BudgetLimits {
    pub(super) tool_calls: u32,
    pub(super) exec_calls: u32,
}

/// A turn's spend against `[limits]`. The counters live for the turn: a new turn
/// of the same agent gets a fresh pair, and the old one is dropped the moment
/// that agent dispatches again.
struct BudgetedTurn {
    budget: Arc<TurnBudget>,
    created: u64,
}

type TurnBudgetKey = (SessionId, AgentId, Option<TurnId>);

static TURN_BUDGETS: LazyLock<Mutex<HashMap<TurnBudgetKey, BudgetedTurn>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static TURN_BUDGET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TurnBudget {
    /// Charge one model-requested call to this turn. Every call is charged, the
    /// refused ones included, so a model cannot spend the budget by retrying a
    /// call that was cut off. `0` in a limit means no limit.
    pub(super) fn reserve(&self, limits: BudgetLimits, is_exec: bool) -> Result<(), String> {
        if limits.tool_calls > 0 {
            let used = self.tool_calls.fetch_add(1, Ordering::Relaxed) + 1;
            if used > limits.tool_calls {
                return Err(format!(
                    "per-turn tool call budget of {} is spent; this call was refused \
                     automatically, not refused by the user — do not retry it; finish the turn \
                     and report what you have so far",
                    limits.tool_calls
                ));
            }
        }
        if is_exec && limits.exec_calls > 0 {
            let used = self.exec_calls.fetch_add(1, Ordering::Relaxed) + 1;
            if used > limits.exec_calls {
                return Err(format!(
                    "per-turn exec budget of {} is spent; this call was refused automatically, not \
                     refused by the user — do not retry it; finish the turn and report what you \
                     have so far",
                    limits.exec_calls
                ));
            }
        }
        Ok(())
    }
}

/// The spending counters of the turn this dispatch belongs to. The agent's
/// current turn id is the epoch: a fresh id means a fresh budget, and a
/// dispatch outside a turn (only tests do that) gets one of its own.
pub(super) fn turn_budget(session: &Session, agent: &Agent) -> Arc<TurnBudget> {
    let key = (session.id.clone(), agent.id(), agent.current_turn());
    let mut budgets = TURN_BUDGETS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    budgets.retain(|held, _| held.0 != key.0 || held.1 != key.1 || held.2 == key.2);
    if let Some(held) = budgets.get(&key) {
        return Arc::clone(&held.budget);
    }
    let budget = Arc::new(TurnBudget::default());
    budgets.insert(
        key,
        BudgetedTurn {
            budget: Arc::clone(&budget),
            created: TURN_BUDGET_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        },
    );
    while budgets.len() > MAX_TRACKED_TURN_BUDGETS {
        let Some(oldest) = budgets
            .iter()
            .min_by_key(|(_, held)| held.created)
            .map(|(held, _)| held.clone())
        else {
            break;
        };
        budgets.remove(&oldest);
    }
    budget
}
