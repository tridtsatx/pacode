//! Output readers of [`TaskManager`] (kept apart to hold `manager.rs` under 500 lines).
//! Both fall back to the spool file for a task evicted by `retention.rs` (F1); the
//! result is empty once the spool was garbage collected (F5).

use pacode_types::TaskId;

use crate::manager::{ExecError, TaskManager};

impl TaskManager {
    /// Last `lines` lines (from RAM tail; falls back to the spool file).
    pub async fn tail(&self, id: &TaskId, lines: usize) -> Result<Vec<String>, ExecError> {
        let (buffer_lines, spool_path) = {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            match tasks.get(id) {
                Some(entry) => (
                    entry.buffer.tail_lines(lines),
                    entry.info.output_path.clone(),
                ),
                // Evicted (F1): read the spool; empty once the spool was GC'd (F5).
                None => match self.state.retention.get(id) {
                    Some(stub) => (Vec::new(), stub.output_path),
                    None => return Err(ExecError::NotFound(id.clone())),
                },
            }
        };

        if !buffer_lines.is_empty() {
            return Ok(buffer_lines);
        }

        match tokio::fs::read_to_string(&spool_path).await {
            Ok(content) => {
                let all_lines: Vec<&str> = content.lines().collect();
                let start = all_lines.len().saturating_sub(lines);
                Ok(all_lines[start..].iter().map(|s| s.to_string()).collect())
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Head+tail rendering capped to `max_chars` (for tool results).
    pub async fn output(&self, id: &TaskId, max_chars: usize) -> Result<String, ExecError> {
        let stub_path = {
            let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
            match tasks.get(id) {
                Some(entry) => return Ok(entry.buffer.render(max_chars)),
                None => self.state.retention.get(id).map(|s| s.output_path),
            }
        };
        match stub_path {
            Some(path) => Ok(crate::retention::spool_tail(&path, max_chars).await),
            None => Err(ExecError::NotFound(id.clone())),
        }
    }
}
