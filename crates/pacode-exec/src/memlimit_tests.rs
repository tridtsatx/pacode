use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use super::*;
use crate::memcap::{LimitKind, PAGE_BYTES};

const GIB: u64 = 1024 * 1024 * 1024;
const MACHINE: u64 = 100 * GIB;

/// The crate locks with `unwrap_or_else(|e| e.into_inner())` rather than
/// poisoning, so one panicking task cannot wedge the others out of the shared
/// state. The fakes match it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Records what it was asked to do instead of touching the kernel, so the
/// routing rules are testable with no cgroup, no process and no allocation.
#[derive(Debug)]
struct FakeBackend {
    kind: LimitKind,
    raises: Mutex<Vec<RaiseTarget>>,
    opened: Mutex<Vec<SessionId>>,
    closed: Mutex<Vec<SessionId>>,
    arm_fails: Mutex<bool>,
    raise_fails: Mutex<bool>,
}

impl FakeBackend {
    fn new(kind: LimitKind) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    fn raises(&self) -> Vec<RaiseTarget> {
        lock(&self.raises).clone()
    }

    fn opened(&self) -> Vec<SessionId> {
        lock(&self.opened).clone()
    }
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            kind: LimitKind::Cgroup,
            raises: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
            arm_fails: Mutex::new(false),
            raise_fails: Mutex::new(false),
        }
    }
}

// On the struct, not on the `Arc`: `with_backend` takes `Arc<dyn LimitBackend>`,
// and that coercion only exists if the struct itself implements the trait.
impl LimitBackend for FakeBackend {
    fn kind(&self) -> LimitKind {
        self.kind
    }

    fn session_opened(&self, session: &SessionId, _ceiling: &MemoryCeiling) {
        lock(&self.opened).push(session.clone());
    }

    fn session_closed(&self, session: &SessionId) {
        lock(&self.closed).push(session.clone());
    }

    fn apply_at_spawn(
        &self,
        _session: &SessionId,
        ceiling: &MemoryCeiling,
    ) -> Result<SpawnSetup, ExecError> {
        if *lock(&self.arm_fails) {
            return Err(ExecError::MemoryLimit(std::io::Error::other("cannot arm")));
        }
        Ok(SpawnSetup::RlimitAs {
            soft: ceiling.bytes,
            hard: ceiling.hard_max_bytes,
        })
    }

    fn raise(&self, target: &RaiseTarget) -> Result<(), ExecError> {
        if *lock(&self.raise_fails) {
            return Err(ExecError::MemoryLimit(std::io::Error::other(
                "no such process",
            )));
        }
        lock(&self.raises).push(target.clone());
        Ok(())
    }
}

fn limiter(backend: Arc<FakeBackend>) -> MemoryLimiter {
    MemoryLimiter::with_backend(
        MemoryPolicy {
            percent: 80,
            split_sessions: true,
            machine_ram_bytes: MACHINE,
        },
        backend,
    )
}

fn session(tag: &str) -> SessionId {
    SessionId::new(format!("pacode-test-{tag}"))
}

fn ask(current: u64, requested: u64) -> RaiseRequest {
    RaiseRequest {
        session: session("a"),
        task: None,
        pid: Some(4242),
        current_bytes: current,
        requested_bytes: requested,
        reason: "could not reserve heap".to_string(),
        tool: "bash".to_string(),
    }
}

fn gate(verdict: RaiseVerdict) -> impl RaiseGate {
    move |_request: RaiseRequest| {
        let verdict = verdict.clone();
        async move { verdict }
    }
}

/// A current-thread runtime, so the async routing can be exercised without a
/// multi-thread runtime in the test build.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

#[test]
fn the_split_follows_the_number_of_live_sessions() {
    let backend = Arc::new(FakeBackend::new(LimitKind::Cgroup));
    let limiter = limiter(Arc::clone(&backend));
    assert_eq!(limiter.live_sessions(), 0);
    assert_eq!(limiter.ceiling().unwrap().bytes, 80 * GIB);

    let first = limiter.session_opened(&session("a"));
    let second = limiter.session_opened(&session("b"));
    assert_eq!(limiter.live_sessions(), 2);
    assert_eq!(limiter.ceiling().unwrap().bytes, 40 * GIB);

    let third = limiter.session_opened(&session("c"));
    let fourth = limiter.session_opened(&session("d"));
    assert_eq!(limiter.live_sessions(), 4);
    assert_eq!(limiter.ceiling().unwrap().bytes, 20 * GIB);

    // Every session that went live also reached the backend, so a cgroup leaf
    // exists for each of them.
    assert_eq!(backend.opened().len(), 4);

    // Ending a session gives its share back rather than leaving the survivors
    // at a third of what the user configured.
    drop(fourth);
    drop(third);
    assert_eq!(limiter.live_sessions(), 2);
    assert_eq!(limiter.ceiling().unwrap().bytes, 40 * GIB);
    drop(second);
    assert_eq!(limiter.live_sessions(), 1);
    assert_eq!(limiter.ceiling().unwrap().bytes, 80 * GIB);
    drop(first);
}

#[test]
fn a_forced_close_tells_the_backend_too() {
    let backend = Arc::new(FakeBackend::new(LimitKind::Cgroup));
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));
    // A hydration of a session this daemon never opened still has to release
    // the cgroup leaf, or the memory stays reserved for nothing.
    limiter.session_closed(&session("a"));
    assert_eq!(limiter.live_sessions(), 0);
    assert_eq!(lock(&backend.closed).len(), 1);
}

#[test]
fn the_soft_limit_is_the_ceiling_and_the_hard_limit_is_the_whole_machine() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    let limiter = limiter(Arc::clone(&backend));
    let first = limiter.session_opened(&session("a"));
    let second = limiter.session_opened(&session("b"));

    let plan = limiter.plan_spawn(&session("a"));
    let ceiling = plan
        .ceiling
        .expect("an enabled policy always has a ceiling");
    assert_eq!(
        plan.setup,
        SpawnSetup::RlimitAs {
            soft: 40 * GIB,
            hard: MACHINE,
        }
    );
    // The tool is told the same two numbers the kernel is given, so a script
    // can read its own ceiling instead of guessing at it.
    assert_eq!(ceiling.bytes, 40 * GIB);
    assert_eq!(ceiling.hard_max_bytes, MACHINE);
    let env = ceiling.environment();
    assert!(env.contains(&(
        "PACODE_MEMORY_LIMIT_BYTES".to_string(),
        (40 * GIB).to_string()
    )));
    assert!(env.contains(&(
        "PACODE_MEMORY_HARD_MAX_BYTES".to_string(),
        MACHINE.to_string()
    )));

    // The real `RLIMIT_AS` backend hands the kernel the very same pair.
    let setup = crate::memrlimit::RlimitBackend::new()
        .apply_at_spawn(&session("a"), &ceiling)
        .expect("rlimit arms");
    assert_eq!(
        setup,
        SpawnSetup::RlimitAs {
            soft: ceiling.bytes,
            hard: MACHINE,
        }
    );
    drop(first);
    drop(second);
}

#[test]
fn a_disabled_cap_produces_no_limit_and_tells_the_tool_nothing() {
    let backend = Arc::new(FakeBackend::new(LimitKind::Cgroup));
    let limiter = MemoryLimiter::with_backend(MemoryPolicy::disabled(), backend);
    let _lease = limiter.session_opened(&session("a"));
    let plan = limiter.plan_spawn(&session("a"));
    assert_eq!(plan.setup, SpawnSetup::None);
    assert_eq!(plan.ceiling, None);
    assert!(limiter.ceiling().is_none());
}

#[test]
fn a_backend_that_cannot_arm_still_lets_the_work_run() {
    let backend = Arc::new(FakeBackend::new(LimitKind::Cgroup));
    *lock(&backend.arm_fails) = true;
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));
    let plan = limiter.plan_spawn(&session("a"));
    // A cap that cannot be armed must not stop the command. The tool is still
    // told the number, so a raise stays possible.
    assert_eq!(plan.setup, SpawnSetup::None);
    assert!(plan.ceiling.is_some());
}

#[test]
fn a_raise_under_bypass_mode_is_not_approved() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));
    let before = limiter.ceiling().unwrap().bytes;

    // Bypass waives approval for what a command *does*. It does not hand out
    // the machine's memory, so its "yes" is not a decision at all.
    let outcome =
        block_on(limiter.request_raise(&gate(RaiseVerdict::Bypass), ask(40 * GIB, 60 * GIB)));

    assert_eq!(
        outcome,
        RaiseOutcome::Denied {
            by: DeniedBy::ApprovalBypass
        }
    );
    assert!(
        backend.raises().is_empty(),
        "bypass must never reach the backend and move a limit"
    );
    assert_eq!(limiter.ceiling().unwrap().bytes, before);
}

#[test]
fn a_raise_the_user_approves_reaches_the_backend() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));

    let outcome = block_on(limiter.request_raise(
        &gate(RaiseVerdict::User {
            grant: Some(60 * GIB),
        }),
        ask(40 * GIB, 60 * GIB),
    ));
    assert_eq!(
        outcome,
        RaiseOutcome::Granted {
            bytes: 60 * GIB,
            kind: LimitKind::AddressSpace
        }
    );
    let raises = backend.raises();
    assert_eq!(raises.len(), 1);
    assert_eq!(raises[0].bytes, 60 * GIB);
    assert_eq!(raises[0].hard_max_bytes, MACHINE);
    assert_eq!(raises[0].pid, Some(4242));
}

#[test]
fn a_refusal_leaves_the_limit_where_it_was() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));
    let outcome = block_on(limiter.request_raise(
        &gate(RaiseVerdict::User { grant: None }),
        ask(40 * GIB, 60 * GIB),
    ));
    assert_eq!(outcome, RaiseOutcome::Denied { by: DeniedBy::User });
    assert!(backend.raises().is_empty());
}

#[test]
fn nothing_can_be_raised_past_the_machines_ram() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));

    // The gate is not consulted at all: there is no answer that would be valid.
    let outcome = block_on(limiter.request_raise(
        &gate(RaiseVerdict::User {
            grant: Some(200 * GIB),
        }),
        ask(40 * GIB, 200 * GIB),
    ));
    assert_eq!(
        outcome,
        RaiseOutcome::AboveHardMax {
            hard_max_bytes: MACHINE
        }
    );
    assert!(backend.raises().is_empty());

    // A request inside the machine that the user answers above it is clamped.
    let outcome = block_on(limiter.request_raise(
        &gate(RaiseVerdict::User {
            grant: Some(200 * GIB),
        }),
        ask(40 * GIB, 90 * GIB),
    ));
    assert_eq!(
        outcome,
        RaiseOutcome::Granted {
            bytes: MACHINE,
            kind: LimitKind::AddressSpace
        }
    );
}

#[test]
fn an_approved_grant_below_what_the_process_has_is_not_a_cut() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));
    let outcome = block_on(limiter.request_raise(
        &gate(RaiseVerdict::User {
            grant: Some(10 * GIB),
        }),
        ask(40 * GIB, 50 * GIB),
    ));
    assert_eq!(
        outcome,
        RaiseOutcome::Granted {
            bytes: 40 * GIB,
            kind: LimitKind::AddressSpace
        }
    );
}

#[test]
fn a_raise_with_no_cap_in_force_does_not_ask_anyone() {
    let backend = Arc::new(FakeBackend::new(LimitKind::Cgroup));
    let limiter = MemoryLimiter::with_backend(MemoryPolicy::disabled(), backend);
    let _lease = limiter.session_opened(&session("a"));
    let asked = Mutex::new(false);
    let counting_gate = |_request: RaiseRequest| {
        *lock(&asked) = true;
        async { RaiseVerdict::User { grant: Some(1) } }
    };
    let outcome = block_on(limiter.request_raise(&counting_gate, ask(0, 8 * GIB)));
    assert_eq!(outcome, RaiseOutcome::Inactive);
    assert!(!*lock(&asked), "no limit, so nothing to ask about");
}

#[test]
fn a_ceiling_is_always_a_whole_number_of_pages() {
    let backend = Arc::new(FakeBackend::new(LimitKind::Cgroup));
    let limiter = limiter(Arc::clone(&backend));
    let leases: Vec<_> = (0..6)
        .map(|index| limiter.session_opened(&session(&format!("s{index}"))))
        .collect();
    let ceiling = limiter.ceiling().unwrap();
    assert_eq!(ceiling.bytes % PAGE_BYTES, 0);
    assert_eq!(ceiling.live_sessions, 6);
    // Six sessions at 80% is 13.33% each, comfortably above the 5% floor.
    assert_eq!(ceiling.bytes, 13 * GIB + 357_912_576);
    drop(leases);
}

#[test]
fn the_decimal_pid_is_written_without_allocating() {
    let mut buffer = [0u8; 12];
    for pid in [1, 9, 10, 99, 100, 12345, 4_194_303] {
        let length = write_decimal(pid, &mut buffer);
        let written = std::str::from_utf8(&buffer[..length]).expect("ascii");
        assert_eq!(written, pid.to_string());
    }
}

#[test]
fn an_approved_raise_the_kernel_refuses_is_not_reported_as_a_refusal() {
    let backend = Arc::new(FakeBackend::new(LimitKind::AddressSpace));
    *lock(&backend.raise_fails) = true;
    let limiter = limiter(Arc::clone(&backend));
    let _lease = limiter.session_opened(&session("a"));
    let outcome = block_on(limiter.request_raise(
        &gate(RaiseVerdict::User {
            grant: Some(60 * GIB),
        }),
        ask(40 * GIB, 60 * GIB),
    ));
    // The user said yes, so a tool that reported this as "denied" would send
    // the model looking for a prompt that was never refused.
    assert!(
        matches!(outcome, RaiseOutcome::ApplyFailed { .. }),
        "{outcome:?}"
    );
    assert!(backend.raises().is_empty());
}
