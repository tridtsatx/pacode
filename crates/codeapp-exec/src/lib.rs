//! Background task manager. Every shell command in codeapp is a task: spawned with
//! pipes in its own process group, output spooled to a file, a bounded head/tail
//! buffer kept in RAM, progress parsed from output or reported by the agent, and a
//! stall watchdog. Foreground vs background is only a matter of who waits.
//!
//! Public surface (implemented in the submodules):
//! - [`TaskManager`]: spawn / info / list / wait / kill / tail / report_progress / subscribe / shutdown
//! - [`TaskSpec`], [`TaskEvent`], [`WaitResult`]
//! - [`HeadTailBuffer`]: bounded output buffer (port of codex `head_tail_buffer`)
//! - [`progress`]: output parsers (cargo, jest/npm, pytest, generic `n/m` and `n%`)

pub mod buffer;
pub mod manager;
pub mod progress;
pub mod spec;

pub use buffer::HeadTailBuffer;
pub use manager::{ExecError, TaskEvent, TaskManager, WaitResult};
pub use spec::TaskSpec;
