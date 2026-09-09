//! Opaque string identifiers. Generated ids are `<prefix>_<12 hex>`: 40 bits of
//! microsecond time plus an 8-bit per-process counter, which is unique enough for
//! one machine and sorts roughly by creation time.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn generate_id(prefix: &str) -> String {
    let micros = crate::time::now_us() & 0xFF_FFFF_FFFF;
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) & 0xFF;
    format!("{prefix}_{micros:010x}{counter:02x}")
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

id_type!(
    /// A session owned by the daemon.
    SessionId,
    "ses"
);
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
mod tests {
    use super::*;

    #[test]
    fn generated_ids_have_prefix_and_are_unique() {
        let a = SessionId::generate();
        let b = SessionId::generate();
        assert!(a.as_str().starts_with("ses_"));
        assert_eq!(a.as_str().len(), 4 + 12);
        assert_ne!(a, b);
    }

    #[test]
    fn main_agent() {
        assert!(AgentId::main().is_main());
        assert!(!AgentId::generate().is_main());
        let json = serde_json::to_string(&AgentId::main()).unwrap();
        assert_eq!(json, "\"main\"");
    }
}
