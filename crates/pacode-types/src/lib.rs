//! Pure data contracts for pacode.
//!
//! Rules for this crate:
//! - no IO, no tokio, no terminal types; only `serde`/`serde_json` as dependencies;
//! - every type here is a wire or storage contract, so serde shape changes are breaking;
//! - helpers are small and pure (formatting, arithmetic over the DTOs).

pub mod at_ref;
pub mod config;
pub mod ids;
pub mod message;
pub mod model;
pub mod protocol;
pub mod state;
pub mod stream;
pub mod time;
pub mod transcript;

pub use at_ref::*;
pub use config::*;
pub use ids::*;
pub use message::*;
pub use model::*;
pub use protocol::*;
pub use state::*;
pub use stream::*;
pub use time::now_ms;
pub use transcript::*;
