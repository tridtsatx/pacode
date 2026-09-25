//! Opaque string identifiers. Generated ids are `<prefix>_<12 hex>`: a 16-bit
//! per-process nonce followed by a 32-bit counter.
//!
//! The nonce is drawn once per process and the counter only ever moves forward,
//! so ids do not repeat inside a process no matter how coarse the clock is or how
//! many are handed out, and the nonce keeps two processes from issuing the same
//! one. Ids are opaque to everything outside this module: nothing parses them,
//! and nothing relies on their order.

use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Nonce in the top 16 bits, counter in the low 32, so a single `fetch_add` walks
/// the whole 48-bit space exactly once: a counter that would run out moves the
/// nonce on instead of wrapping back onto a value already issued.
static SEQUENCE: OnceLock<AtomicU64> = OnceLock::new();

fn next_sequence() -> u64 {
    SEQUENCE
        .get_or_init(|| AtomicU64::new(u64::from(rand::random::<u16>()) << 32))
        .fetch_add(1, Ordering::Relaxed)
}

fn generate_id(prefix: &str) -> String {
    let sequence = next_sequence();
    let nonce = (sequence >> 32) as u16;
    let counter = sequence as u32;
    format!("{prefix}_{nonce:04x}{counter:08x}")
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn generate() -> Self {
                Self(generate_id($prefix))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

/// A session owned by the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub const PREFIX: &'static str = "pacode";

    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Random rather than counted: a session id outlives the process that made
    /// it and a client resumes by naming it, so two sessions must not collide
    /// across restarts and an id must not be guessable from another one.
    pub fn generate() -> Self {
        use rand::Rng;
        let mut rng = rand::rng();
        let a: u32 = rng.random();
        let b: u16 = rng.random();
        Self(format!("pacode-{a:08x}-{b:04x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

id_type!(
    /// An agent inside a session. The main agent is always [`AgentId::main`].
    AgentId,
    "agt"
);
id_type!(
    /// One turn of one agent.
    TurnId,
    "trn"
);
id_type!(
    /// A tool call id. Providers may supply their own; otherwise generated.
    CallId,
    "call"
);
id_type!(
    /// A background task (process) owned by a session.
    TaskId,
    "tsk"
);
id_type!(
    /// A pending permission request.
    PermissionId,
    "perm"
);
id_type!(
    /// A connected client process.
    ClientId,
    "cli"
);
id_type!(
    /// A question the model asked the user; the turn waits for its answer.
    QuestionId,
    "qst"
);
id_type!(
    /// A scheduled prompt (cron job). Persisted; survives a daemon restart.
    CronJobId,
    "cron"
);
id_type!(
    /// A condition watcher owned by a session. In memory only.
    MonitorId,
    "mon"
);

impl AgentId {
    pub const MAIN: &'static str = "main";

    pub fn main() -> Self {
        Self(Self::MAIN.to_string())
    }

    pub fn is_main(&self) -> bool {
        self.0 == Self::MAIN
    }
}

#[cfg(test)]
#[path = "ids_tests.rs"]
mod ids_tests;
