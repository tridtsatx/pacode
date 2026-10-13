//! Background task manager. Every shell command in pacode is a task: spawned with
//! pipes in its own process group, output spooled to a file, a bounded head/tail
//! buffer kept in RAM, progress parsed from output or reported by the agent, and a
//! stall watchdog. Foreground vs background is only a matter of who waits.
//!
//! Every task is also capped in memory, so a model cannot launch a script that
//! allocates the whole machine and takes the desktop down with an OOM kill.
//! See [`memcap`] for the arithmetic, [`memlimit`] for the runtime and the
//! raise flow, [`memcgroup`] / [`memrlimit`] for the two enforcement
//! mechanisms, and [`memsessions`] for the live-session division.
//!
//! Public surface (implemented in the submodules):
//! - [`TaskManager`]: spawn / info / list / wait / kill / tail / report_progress / subscribe / shutdown
//! - [`TaskSpec`], [`TaskEvent`], [`WaitResult`]
//! - [`HeadTailBuffer`]: bounded output buffer (port of codex `head_tail_buffer`)
//! - [`progress`]: output parsers (cargo, jest/npm, pytest, generic `n/m` and `n%`)
//! - [`MemoryLimiter`]: tool memory ceiling, live-session split, raise routing

pub mod buffer;
pub mod manager;
pub mod memcap;
pub mod memcgroup;
pub mod memlimit;
pub mod memrlimit;
pub mod memsessions;
pub mod oneshot;
pub mod progress;
pub mod spec;
mod state;
mod supervisor;

pub use buffer::HeadTailBuffer;
pub use manager::{ExecError, ForegroundGuard, TaskEvent, TaskManager, WaitResult};
pub use memcap::{LimitKind, MemoryCeiling, MemoryPolicy};
pub use memlimit::{
    DeniedBy, LimitBackend, MemoryLimiter, RaiseGate, RaiseOutcome, RaiseRequest, RaiseTarget,
    RaiseVerdict, SpawnPlan, SpawnSetup,
};
pub use memsessions::{SessionLease, SessionRegistry};
pub use oneshot::{CapturedOutput, run_capture};
pub use spec::TaskSpec;
