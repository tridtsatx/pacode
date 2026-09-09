//! Touched files tracking for the files overlay (spec §11).

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;

pub const MAX_FILES: usize = 500;

/// Bitflags for operations performed on a file: Read, Write, Edit, Search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileKinds(pub u8);

impl FileKinds {
    pub const READ: u8 = 1 << 0;
    pub const WRITE: u8 = 1 << 1;
    pub const EDIT: u8 = 1 << 2;
    pub const SEARCH: u8 = 1 << 3;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn add(&mut self, flag: u8) {
        self.0 |= flag;
    }

    pub fn contains(&self, flag: u8) -> bool {
        (self.0 & flag) != 0
    }

    pub fn has_read(&self) -> bool {
        self.contains(Self::READ)
    }

    pub fn has_write(&self) -> bool {
        self.contains(Self::WRITE)
    }

    pub fn has_edit(&self) -> bool {
        self.contains(Self::EDIT)
    }

    pub fn has_search(&self) -> bool {
        self.contains(Self::SEARCH)
    }
}

/// One row in the files overlay table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub kinds: FileKinds,
    pub count: u32,
    pub last_ts_ms: u64,
}

/// State tracking files touched by tools during the session.
#[derive(Clone, Debug, Default)]
pub struct FilesState {
    pub rows: Vec<FileRow>,
}

impl FilesState {
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Observe a tool item call and update tracked files.
    /// MCP tools (containing `__`) are ignored.
    /// Tool names:
    /// - `read_file` / `read` -> R
    /// - `write_file` / `write` -> W
    /// - `edit_file` / `edit` / `apply_patch` / `multi_edit` -> E
    /// - `grep` / `glob` / `search` / `ls` -> S
    pub fn observe_tool_item(&mut self, tool_name: &str, input: &serde_json::Value, ts_ms: u64) {
        if tool_name.contains("__") {
            return;
        }

        let kind_flag = match tool_name {
            "read_file" | "read" => FileKinds::READ,
            "write_file" | "write" => FileKinds::WRITE,
            "edit_file" | "edit" | "apply_patch" | "multi_edit" => FileKinds::EDIT,
            "grep" | "glob" | "search" | "ls" => FileKinds::SEARCH,
            _ => return,
        };

        let paths = extract_paths(input);
        for path in paths {
            if path.is_empty() {
                continue;
            }
            if let Some(row) = self.rows.iter_mut().find(|r| r.path == path) {
                row.kinds.add(kind_flag);
                row.count = row.count.saturating_add(1);
                row.last_ts_ms = ts_ms;
            } else {
                let mut kinds = FileKinds::new();
                kinds.add(kind_flag);
                self.rows.push(FileRow {
                    path,
                    kinds,
                    count: 1,
                    last_ts_ms: ts_ms,
                });
                self.enforce_cap();
            }
        }
    }

    /// Return references to all rows sorted by `last_ts_ms` descending.
    pub fn sorted_rows(&self) -> Vec<&FileRow> {
        let mut sorted: Vec<&FileRow> = self.rows.iter().collect();
        sorted.sort_by(|a, b| {
            b.last_ts_ms
                .cmp(&a.last_ts_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        sorted
    }

    fn enforce_cap(&mut self) {
        while self.rows.len() > MAX_FILES {
            if let Some((min_idx, _)) = self
                .rows
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| r.last_ts_ms)
            {
                self.rows.remove(min_idx);
            } else {
                break;
            }
        }
    }
}

fn extract_paths(input: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(obj) = input.as_object() else {
        return out;
    };

    for key in ["path", "file_path", "dir"] {
        if let Some(val) = obj.get(key)
            && let Some(s) = val.as_str()
            && !s.is_empty()
        {
            out.push(s.to_string());
        }
    }

    if let Some(val) = obj.get("paths") {
        if let Some(arr) = val.as_array() {
            for item in arr {
                if let Some(s) = item.as_str()
                    && !s.is_empty()
                {
                    out.push(s.to_string());
                }
            }
        } else if let Some(s) = val.as_str()
            && !s.is_empty()
        {
            out.push(s.to_string());
        }
    }

    out
}
