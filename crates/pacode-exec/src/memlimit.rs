//! The runtime half: pick a backend, hold the policy, split the ceiling across
//! live sessions, and route a raise request to a human.
//!
//! # Backend choice
//!
//! [`MemoryLimiter::new`] probes once, at daemon start, and picks:
//!
//! 1. **cgroup v2**, when this process can build a memory-limited subtree of
//!    its own ([`crate::memcgroup`]). It counts resident pages, so a JIT runtime
//!    is charged for what it touches rather than for what it reserved, and
//!    `memory.max` is a file, so raising a live process is a rewrite.
//! 2. **`RLIMIT_AS`**, otherwise ([`crate::memrlimit`]). Real, kernel-enforced
//!    and raisable without privilege, but measured against virtual address
//!    space rather than resident pages.
//!
//! The probe is one-time and costs four file operations. There is no timer, no
//! retry loop and no polling: a backend that works keeps working, and one that
//! stops working fails the raise loudly rather than being re-probed.
//!
//! # What the ceiling is split across
//!
//! Sessions, not tasks. `SessionRegistry` holds the live set and a session
//! takes a share when it opens; the share is recomputed on every spawn, so a
//! session that ends immediately hands its share back to the ones still running
//! without anything having to be told twice.
//!
//! # Raising
//!
//! A raise is a *permission*, not an action. Bypass mode waives approval for
//! what a command does; it does not hand out the machine's memory. A bypassed
//! mode answers [`RaiseVerdict::Bypass`], which [`MemoryLimiter::request_raise`]
//! treats as no answer at all: the limit does not move. Only
//! [`RaiseVerdict::User`] can move it, and never past the machine's RAM.

use std::ffi::{CStr, CString};
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

use pacode_types::{SessionId, TaskId};

use crate::manager::ExecError;
use crate::memcap::{LimitKind, MemoryCeiling, MemoryPolicy};
use crate::memsessions::{SessionLease, SessionRegistry};

/// What the child has to do between fork and exec. Kept as data rather than as
/// a closure so the decision is inspectable and testable without a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnSetup {
    /// No cap on this spawn.
    None,
    /// `setrlimit(RLIMIT_AS, soft, hard)` in the child.
    RlimitAs { soft: u64, hard: u64 },
    /// The child writes its own pid into the session's `cgroup.procs`, which is
    /// how a process joins a cgroup it is already a member of a tree under. Done
    /// in the child rather than by the parent writing the pid afterwards: the
    /// child cannot allocate before it has been moved, so there is no window in
    /// which a forked process is briefly unaccounted for. The path is a
    /// `CString` because this runs between fork and exec, where allocating is a
    /// deadlock waiting to happen.
    CgroupJoin { procs: CString },
}

impl SpawnSetup {
    /// Install the setup in a forked child. Called from `pre_exec`, where the
    /// only legal things are raw syscalls on already-allocated memory, so this
    /// path allocates nothing: the file name, the decimal pid buffer and the
    /// limit struct are all fixed size.
    pub fn install(&self) -> io::Result<()> {
        match self {
            Self::None => Ok(()),
            Self::RlimitAs { soft, hard } => crate::memrlimit::apply_in_child(*soft, *hard),
            Self::CgroupJoin { procs } => join_cgroup(procs),
        }
    }
}

/// `open`/`write`/`close` on the cgroup's `cgroup.procs` with the child's own
/// pid. Every call is a bare syscall; nothing here can take a lock or touch
/// the allocator, which is the only thing `pre_exec` tolerates.
#[cfg(target_os = "linux")]
fn join_cgroup(procs: &CStr) -> io::Result<()> {
    // SAFETY: `procs` is NUL-terminated and owned by the caller, and each
    // pointer below refers to memory that outlives the call.
    let fd = unsafe { libc::open(procs.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = [0u8; 12];
    let length = write_decimal(unsafe { libc::getpid() }, &mut buffer);
    // SAFETY: `fd` is open and `buffer[..length]` is initialised.
    let written = unsafe { libc::write(fd, buffer.as_ptr().cast(), length) };
    // SAFETY: `fd` came from `open` above and is closed exactly once.
    unsafe { libc::close(fd) };
    if written == length as isize {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Decimal digits into a caller-supplied buffer, no allocation.
fn write_decimal(mut value: libc::pid_t, buffer: &mut [u8; 12]) -> usize {
    let mut length = 0;
    let mut digits = [0u8; 12];
    loop {
        digits[length] = b'0' + (value.unsigned_abs() % 10) as u8;
        length += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for index in 0..length {
        buffer[index] = digits[length - 1 - index];
    }
    length
}

#[cfg(not(target_os = "linux"))]
fn join_cgroup(_procs: &std::ffi::CStr) -> io::Result<()> {
    Ok(())
}

/// Where a raise is aimed. The cgroup backend addresses the session's group
/// and ignores the pid; the rlimit backend needs a pid and has no group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiseTarget {
    pub session: SessionId,
    pub pid: Option<u32>,
    pub bytes: u64,
    pub hard_max_bytes: u64,
}

/// A limit mechanism. Faked in tests; two real implementations exist.
pub trait LimitBackend: fmt::Debug + Send + Sync {
    fn kind(&self) -> LimitKind;

    /// A session started. The backend may pre-create its group.
    fn session_opened(&self, _session: &SessionId, _ceiling: &MemoryCeiling) {}

    /// A session ended. Freeing resources here must not be able to fail the
    /// caller: the session is gone either way.
    fn session_closed(&self, _session: &SessionId) {}

    /// What this process must do before exec to be under `ceiling`.
    fn apply_at_spawn(
        &self,
        session: &SessionId,
        ceiling: &MemoryCeiling,
    ) -> Result<SpawnSetup, ExecError>;

    /// Move a live process's limit up to `target.bytes`.
    fn raise(&self, target: &RaiseTarget) -> Result<(), ExecError>;
}

/// A request for more room, as a tool makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiseRequest {
    pub session: SessionId,
    /// The pacode task making the ask, so the prompt can name what is stuck.
    pub task: Option<TaskId>,
    /// The process to raise, when the asker knows it. The address-space backend
    /// needs it; the cgroup backend raises the whole session's group.
    pub pid: Option<u32>,
    /// What the process has right now. A grant below this is not a raise, so it
    /// is rounded up rather than applied as a cut.
    pub current_bytes: u64,
    pub requested_bytes: u64,
    /// Shown to the user, e.g. `could not reserve heap`.
    pub reason: String,
    /// The tool that asked, for the prompt's subject line.
    pub tool: String,
}

/// What came back from the permission side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiseVerdict {
    /// A human answered. `None` is a refusal.
    User { grant: Option<u64> },
    /// An approval mode that waives permission for actions is being offered as
    /// if it applied to a machine-level limit. It does not, and this variant
    /// exists so that a mode which returns it can never be mistaken for consent.
    Bypass,
}

/// The permission side of a raise. The real implementation forwards to the
/// client's approval prompt; the trait exists so the routing rule is testable
/// without a TUI.
pub trait RaiseGate: Send + Sync {
    fn request(
        &self,
        request: RaiseRequest,
    ) -> Pin<Box<dyn Future<Output = RaiseVerdict> + Send + '_>>;
}

impl<F, Fut> RaiseGate for F
where
    F: Fn(RaiseRequest) -> Fut + Send + Sync,
    // `'static` because the trait boxes the future against `&self`'s lifetime:
    // a future that borrowed the gate could not be spelled here at all. The
    // gate is `Send + Sync` and lives behind an `Arc` anyway, so it has nothing
    // to borrow from.
    Fut: Future<Output = RaiseVerdict> + Send + 'static,
{
    fn request(
        &self,
        request: RaiseRequest,
    ) -> Pin<Box<dyn Future<Output = RaiseVerdict> + Send + '_>> {
        Box::pin(self(request))
    }
}

/// Why a raise did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiseOutcome {
    Granted {
        bytes: u64,
        kind: LimitKind,
    },
    Denied {
        by: DeniedBy,
    },
    /// Approved, and the kernel would not take it. Not a refusal: a tool that
    /// reports this as "the user said no" sends the model looking in the wrong
    /// place.
    ApplyFailed {
        reason: String,
    },
    /// No cap is in force, so there is nothing to raise.
    Inactive,
    /// More than the machine has. A human cannot grant it and neither can
    /// `prlimit`: the hard limit is physical RAM.
    AboveHardMax {
        hard_max_bytes: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeniedBy {
    /// The user said no.
    User,
    /// An approval mode answered instead of a human. Treated as no answer.
    ApprovalBypass,
}

/// The result of planning a spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnPlan {
    /// What the child does before exec.
    pub setup: SpawnSetup,
    /// `None` when the cap is off. When set, its [`MemoryCeiling::environment`]
    /// goes into the process so the tool can see the ceiling it works under.
    pub ceiling: Option<MemoryCeiling>,
}

struct MemoryInner {
    policy: MemoryPolicy,
    sessions: SessionRegistry,
    backend: Arc<dyn LimitBackend>,
}

/// Owns the tool memory policy for one daemon. Cheap to clone; the clone is
/// the same policy, not a copy of it, so a raise through any clone is visible
/// to every spawn.
#[derive(Clone)]
pub struct MemoryLimiter {
    inner: Arc<MemoryInner>,
}

impl fmt::Debug for MemoryLimiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryLimiter")
            .field("policy", &self.inner.policy)
            .field("backend", &self.inner.backend.kind())
            .field("live_sessions", &self.inner.sessions.live())
            .finish()
    }
}

impl MemoryLimiter {
    /// Probe once and build a limiter. Never fails: if nothing can be enforced
    /// the policy is simply inactive.
    pub fn new(policy: MemoryPolicy) -> Self {
        if !policy.is_enabled() {
            return Self::with_backend(policy, Arc::new(crate::memrlimit::RlimitBackend::new()));
        }
        let backend: Arc<dyn LimitBackend> = match crate::memcgroup::CgroupV2::probe(&policy) {
            Ok(cgroup) => Arc::new(cgroup),
            Err(reason) => {
                log::warn!(
                    "tool memory: cgroup v2 unavailable ({reason}); \
                     falling back to RLIMIT_AS, which caps virtual address space"
                );
                Arc::new(crate::memrlimit::RlimitBackend::new())
            }
        };
        Self::with_backend(policy, backend)
    }

    pub fn with_backend(policy: MemoryPolicy, backend: Arc<dyn LimitBackend>) -> Self {
        Self {
            inner: Arc::new(MemoryInner {
                policy,
                sessions: SessionRegistry::new(),
                backend,
            }),
        }
    }

    pub fn policy(&self) -> &MemoryPolicy {
        &self.inner.policy
    }

    pub fn backend_kind(&self) -> LimitKind {
        self.inner.backend.kind()
    }

    /// How many sessions the ceiling is currently divided between.
    pub fn live_sessions(&self) -> usize {
        self.inner.sessions.live()
    }

    /// A session went live. Hold the returned lease for as long as the session
    /// is; the share is given back when it drops.
    pub fn session_opened(&self, session: &SessionId) -> SessionLease {
        let lease = self.inner.sessions.open(session);
        if let Some(ceiling) = self.ceiling() {
            self.inner.backend.session_opened(session, &ceiling);
        }
        lease
    }

    /// A session ended without its lease being held (a forced end, a hydration
    /// of a session the daemon never opened). Idempotent.
    pub fn session_closed(&self, session: &SessionId) {
        self.inner.sessions.close(session);
        self.inner.backend.session_closed(session);
    }

    /// The ceiling a process gets right now, and the hard limit it may be
    /// raised to. `None` means no cap.
    ///
    /// Deliberately takes no session: the share is the ceiling divided by the
    /// number of *live* sessions, so every session currently gets the same one
    /// and a per-session argument here would invite the belief that it does
    /// not. Which session is asking matters to the backend that installs the
    /// limit, and that arrives separately.
    pub fn ceiling(&self) -> Option<MemoryCeiling> {
        let live = self.inner.sessions.live();
        // Hoisted out of the `map` below: the hard limit is a property of the
        // policy, not of the computed share, and it is also what says there is
        // a cap at all — a policy with no hard limit has nothing to raise toward.
        let hard_max_bytes = self.inner.policy.hard_max_bytes()?;
        self.inner
            .policy
            .ceiling_for(live)
            .map(|bytes| MemoryCeiling {
                bytes,
                hard_max_bytes,
                kind: self.inner.backend.kind(),
                live_sessions: live,
                percent: self.inner.policy.percent,
                split_sessions: self.inner.policy.split_sessions,
                machine_ram_bytes: self.inner.policy.machine_ram_bytes,
            })
    }

    /// Plan one spawn: what the child does before exec, and what the tool is
    /// told. A backend that refuses to arm is logged and the process runs
    /// uncapped — a cap that cannot be armed must not stop the work.
    pub fn plan_spawn(&self, session: &SessionId) -> SpawnPlan {
        let Some(ceiling) = self.ceiling() else {
            return SpawnPlan {
                setup: SpawnSetup::None,
                ceiling: None,
            };
        };
        match self.inner.backend.apply_at_spawn(session, &ceiling) {
            Ok(setup) => SpawnPlan {
                setup,
                ceiling: Some(ceiling),
            },
            Err(error) => {
                log::warn!("tool memory: could not arm the cap for this spawn: {error}");
                SpawnPlan {
                    setup: SpawnSetup::None,
                    ceiling: Some(ceiling),
                }
            }
        }
    }

    /// Ask for more room. Never self-approves: the answer comes from `gate`,
    /// and an answer that is not a human's is not an answer.
    pub async fn request_raise(&self, gate: &dyn RaiseGate, request: RaiseRequest) -> RaiseOutcome {
        let Some(hard_max_bytes) = self.inner.policy.hard_max_bytes() else {
            return RaiseOutcome::Inactive;
        };
        if request.requested_bytes > hard_max_bytes {
            return RaiseOutcome::AboveHardMax { hard_max_bytes };
        }
        let granted = match gate.request(request.clone()).await {
            RaiseVerdict::User {
                grant: Some(granted),
            } => granted,
            RaiseVerdict::Bypass => {
                return RaiseOutcome::Denied {
                    by: DeniedBy::ApprovalBypass,
                };
            }
            RaiseVerdict::User { grant: None } => {
                return RaiseOutcome::Denied { by: DeniedBy::User };
            }
        };
        let bytes = granted.min(hard_max_bytes).max(request.current_bytes);
        let target = RaiseTarget {
            session: request.session.clone(),
            pid: request.pid,
            bytes,
            hard_max_bytes,
        };
        match self.inner.backend.raise(&target) {
            Ok(()) => RaiseOutcome::Granted {
                bytes,
                kind: self.inner.backend.kind(),
            },
            Err(error) => {
                log::warn!("tool memory: approved raise could not be applied: {error}");
                RaiseOutcome::ApplyFailed {
                    reason: error.to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "memlimit_tests.rs"]
mod memlimit_tests;
