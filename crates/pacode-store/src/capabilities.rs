use pacode_types::{AgentId, SessionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StorageDialect {
    Sqlite,
    Turso,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JournalMode {
    Delete,
    Truncate,
    Persist,
    Memory,
    Wal,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SynchronousMode {
    Off,
    Normal,
    Full,
    Extra,
    Other,
}

/// What a backend has to offer for pacode to run on it. These are capabilities,
/// not settings: `synchronous` is deliberately not one of them, because how
/// durable a commit is a choice pacode makes for itself (see
/// [`StorageCapabilities::synchronous`]) and pinning it here would only mean the
/// store can no longer pick the durability level its throughput needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialectRequirements {
    pub fts5: bool,
    /// Write-ahead logging, which is what lets the store's single writer commit
    /// without blocking the readers.
    pub wal: bool,
    pub transactions: bool,
    pub keyset_pagination: bool,
}

impl DialectRequirements {
    pub const PACODE: Self = Self {
        fts5: true,
        wal: true,
        transactions: true,
        keyset_pagination: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityGap {
    Fts5,
    Wal,
    Transactions,
    KeysetPagination,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageCapabilities {
    pub dialect: StorageDialect,
    pub fts5: bool,
    pub journal_mode: JournalMode,
    /// Reported, not required: the live `synchronous` setting of the connection.
    /// Under WAL this is what decides whether a commit reaches the disk
    /// (`Full`) or the WAL (`Normal`), and pacode runs at `Normal` because the
    /// cost of `Full` is paid by every caller in the store's batch.
    pub synchronous: SynchronousMode,
    pub transactions: bool,
    pub keyset_pagination: bool,
}

impl StorageCapabilities {
    pub fn satisfies(&self, requirements: &DialectRequirements) -> bool {
        self.gaps(requirements).is_empty()
    }

    pub fn gaps(&self, requirements: &DialectRequirements) -> Vec<CapabilityGap> {
        let mut gaps = Vec::new();
        if requirements.fts5 && !self.fts5 {
            gaps.push(CapabilityGap::Fts5);
        }
        if requirements.wal && self.journal_mode != JournalMode::Wal {
            gaps.push(CapabilityGap::Wal);
        }
        if requirements.transactions && !self.transactions {
            gaps.push(CapabilityGap::Transactions);
        }
        if requirements.keyset_pagination && !self.keyset_pagination {
            gaps.push(CapabilityGap::KeysetPagination);
        }
        gaps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeysetCursor {
    pub value: u64,
}

impl KeysetCursor {
    pub const fn new(value: u64) -> Self {
        Self { value }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeysetPage {
    pub before: Option<KeysetCursor>,
    pub limit: u32,
}

impl KeysetPage {
    pub const DEFAULT_LIMIT: u32 = 200;

    pub const fn new(before: Option<KeysetCursor>, limit: u32) -> Self {
        Self { before, limit }
    }

    pub const fn normalized(self) -> Self {
        Self {
            before: self.before,
            limit: if self.limit == 0 {
                Self::DEFAULT_LIMIT
            } else {
                self.limit
            },
        }
    }

    pub const fn next_page(self, cursor: KeysetCursor) -> Self {
        Self {
            before: Some(cursor),
            limit: if self.limit == 0 {
                Self::DEFAULT_LIMIT
            } else {
                self.limit
            },
        }
    }
}

impl Default for KeysetPage {
    fn default() -> Self {
        Self::new(None, Self::DEFAULT_LIMIT)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<KeysetCursor>,
}

impl<T> QueryPage<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<KeysetCursor>) -> Self {
        Self { items, next_cursor }
    }

    pub fn has_more(&self) -> bool {
        self.next_cursor.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageHistoryQuery {
    pub session: SessionId,
    pub agent: AgentId,
    pub page: KeysetPage,
}

impl MessageHistoryQuery {
    pub fn new(
        session: SessionId,
        agent: AgentId,
        before: Option<KeysetCursor>,
        limit: u32,
    ) -> Self {
        Self {
            session,
            agent,
            page: KeysetPage::new(before, limit),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub limit: u32,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>, limit: u32) -> Self {
        Self {
            text: text.into(),
            limit,
        }
    }

    pub const fn effective_limit(&self) -> u32 {
        if self.limit == 0 {
            KeysetPage::DEFAULT_LIMIT
        } else {
            self.limit
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkOperation {
    Fts5Search,
    TransactionCommit,
    TransactionRollback,
    KeysetPage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkPlan {
    pub operation: BenchmarkOperation,
    pub iterations: u32,
    pub page_size: u32,
}

impl BenchmarkPlan {
    pub const fn new(operation: BenchmarkOperation, iterations: u32, page_size: u32) -> Self {
        Self {
            operation,
            iterations,
            page_size,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkResult {
    pub operation: BenchmarkOperation,
    pub iterations: u32,
    pub elapsed_us: u64,
    pub rows: u64,
}

pub trait StorageAdapter: Send + Sync {
    type Error;
    type Message;
    type SearchHit;

    fn capabilities(&self) -> StorageCapabilities;

    fn message_history(
        &self,
        query: &MessageHistoryQuery,
    ) -> Result<QueryPage<Self::Message>, Self::Error>;

    fn search(&self, query: &SearchQuery) -> Result<Vec<Self::SearchHit>, Self::Error>;

    fn benchmark(&self, plan: BenchmarkPlan) -> Result<BenchmarkResult, Self::Error>;
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;
