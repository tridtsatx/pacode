use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use codeapp_types::{ExecConfig, TaskId, TaskInfo};
use tokio::sync::broadcast;

use crate::buffer::HeadTailBuffer;
use crate::manager::TaskEvent;
use crate::progress::ProgressParser;

pub(crate) struct TaskEntry {
    pub(crate) info: TaskInfo,
    pub(crate) buffer: HeadTailBuffer,
    pub(crate) parser: ProgressParser,
    pub(crate) version_tx: tokio::sync::watch::Sender<u64>,
    pub(crate) last_activity: std::time::Instant,
    pub(crate) stalled: bool,
    pub(crate) kill_requested: bool,
    pub(crate) child_pid: Option<u32>,
    pub(crate) supervisor: Option<tokio::task::JoinHandle<()>>,
}

pub(crate) struct SharedState {
    pub(crate) spool_dir: PathBuf,
    pub(crate) config: ExecConfig,
    pub(crate) tasks: Mutex<HashMap<TaskId, TaskEntry>>,
    pub(crate) events: broadcast::Sender<TaskEvent>,
}
