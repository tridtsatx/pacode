//! `pacode-auth` — OAuth and credential storage core for pacode.
//!
//! This crate provides the low-level authentication primitives required by pacode:
//! - [`pkce`]: PKCE (RFC 7636) code verifier and challenge generation (`S256`), plus CSRF `random_state`.
//! - [`callback`]: Loopback OAuth callback server on Tokio with self-contained success/failure HTML pages.
//! - [`store`]: Atomic, secure JSON credential store (`auth.json`) with multi-account support.
//! - [`refresh`]: Single-flight refresh de-duplication and terminal failure tracking.
//! - [`browser`]: Detached platform-native browser launcher.
//! - [`catalog`]: Static login provider descriptor table and resolver.
//! - [`error`]: Strongly typed `AuthError` enum based on `thiserror`.

pub mod browser;
pub mod callback;
pub mod catalog;
pub mod error;
pub mod flows;
pub mod import;
pub mod pkce;
pub mod refresh;
pub mod store;

pub use browser::{build_browser_command, open_url};
pub use callback::{CallbackListener, CallbackParams, bind_callback};
pub use catalog::{AuthKind, LOGIN_PROVIDERS, LoginProvider, find};
pub use error::{AuthError, Result};
pub use flows::{LoginOutcome, Progress, access_token, access_token_in, login};
pub use pkce::{Pkce, compute_challenge, random_state};
pub use refresh::{RefreshState, SingleFlight};
pub use store::{Account, AuthStore, AuthStoreData, ProviderEntry};
