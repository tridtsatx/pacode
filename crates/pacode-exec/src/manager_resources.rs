//! Resource-guard surface of [`TaskManager`] (kept apart to hold `manager.rs` under
//! 500 lines).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pacode_types::{ExecConfig, ResourcesConfig, TaskId};
use tokio::sync::broadcast;

use crate::manager::{ExecError, TaskManager};
use crate::resources::{GuardHost, ResourceGuard};
use crate::state::SharedState;

impl TaskManager {
    pub fn with_resources(
        spool_dir: PathBuf,
        config: ExecConfig,
        resources: ResourcesConfig,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let guard = ResourceGuard::new(&resources, spool_dir.clone());
        let state = Arc::new(SharedState {
            spool_dir,
            config,
            tasks: Mutex::new(HashMap::new()),
            events,
            guard,
            nice: resources.nice,
        });
        let host: Arc<dyn GuardHost> = state.clone();
        state.guard.bind(Arc::downgrade(&host));
        Arc::new(Self { state })
    }

    /// Refuse a new subagent when machine resources are low.
    pub fn admit_agent(&self) -> Result<(), ExecError> {
        self.state.guard.admit_agent()
    }

    pub fn guard(&self) -> &ResourceGuard {
        &self.state.guard
    }

    /// Exempt (or re-include) a task tree from resource-guard kills.
    pub fn set_protected(&self, id: &TaskId, protected: bool) {
        let mut tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tasks.get_mut(id) {
            entry.protected = protected;
        }
    }
}
