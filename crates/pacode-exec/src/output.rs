//! Reading a task's output back: the in-RAM tail, or the spool file when the
//! buffer no longer holds it.

use std::io::SeekFrom;
#[cfg(any(target_os = "android", target_os = "linux"))]
use std::os::fd::AsRawFd;

use std::sync::Arc;

use pacode_types::TaskId;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{ExecError, TaskManager};
use crate::state::ManagedSpool;

impl TaskManager {
    pub async fn tail(&self, id: &TaskId, lines: usize) -> Result<Vec<String>, ExecError> {
        let (buffer_lines, spool) = {
            let tasks = self
                .state
                .tasks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let entry = tasks
                .get(id)
                .ok_or_else(|| ExecError::NotFound(id.clone()))?;
            (entry.buffer.tail_lines(lines), Arc::clone(&entry.spool))
        };

        if lines == 0 || !buffer_lines.is_empty() {
            return Ok(buffer_lines);
        }
        Ok(tail_spool(
            spool.as_ref(),
            lines,
            self.state.config.tail_bytes.max(MIN_TAIL_SPOOL_BYTES),
        )
        .await)
    }

    /// Head+tail rendering capped to `max_chars` (for tool results).
    pub async fn output(&self, id: &TaskId, max_chars: usize) -> Result<String, ExecError> {
        let tasks = self.state.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let entry = tasks
            .get(id)
            .ok_or_else(|| ExecError::NotFound(id.clone()))?;
        Ok(entry.buffer.render(max_chars))
    }
}

const MIN_TAIL_SPOOL_BYTES: usize = 64 * 1024;

#[cfg(any(target_os = "android", target_os = "linux"))]
fn first_data_offset(file: &tokio::fs::File, offset: u64) -> u64 {
    let Ok(offset) = libc::off_t::try_from(offset) else {
        return offset;
    };
    let original = u64::try_from(offset).unwrap_or_default();
    let data_offset = unsafe { libc::lseek(file.as_raw_fd(), offset, libc::SEEK_DATA) };
    if data_offset < 0 {
        original
    } else {
        u64::try_from(data_offset).unwrap_or(original)
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn first_data_offset(_file: &tokio::fs::File, offset: u64) -> u64 {
    offset
}

async fn tail_spool(spool: &ManagedSpool, lines: usize, max_bytes: usize) -> Vec<String> {
    if lines == 0 || max_bytes == 0 {
        return Vec::new();
    }
    let mut file = match tokio::fs::File::open(spool.path()).await {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let file_len = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return Vec::new(),
    };
    let max_bytes = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let read_len = file_len.min(max_bytes);
    if read_len == 0 {
        return Vec::new();
    }
    let requested_start = file_len - read_len;
    let start = first_data_offset(&file, requested_start).min(file_len);
    let read_len = file_len - start;
    if read_len == 0 {
        return Vec::new();
    }
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return Vec::new();
    }
    let capacity = usize::try_from(read_len).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    if file.take(read_len).read_to_end(&mut bytes).await.is_err() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&bytes);
    let text = if start != requested_start {
        text.trim_start_matches('\0')
    } else if start > 0 {
        match text.find('\n') {
            Some(newline) => &text[newline + 1..],
            None => text.as_ref(),
        }
    } else {
        text.as_ref()
    };
    let mut result = text
        .lines()
        .rev()
        .take(lines)
        .map(str::to_string)
        .collect::<Vec<_>>();
    result.reverse();
    result
}
