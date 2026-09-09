//! ACP (Agent Client Protocol) adapter for pacode.
//!
//! Enables pacode to act as an external ACP agent for editors such as Zed.

pub mod error;
pub mod mapping;
pub mod server;
pub mod session;

pub use error::AcpError;
pub use server::run_server;

use pacode_client::ClientOptions;
use pacode_types::Config;

/// Runs the ACP agent server with a dedicated multi-thread tokio runtime.
pub fn run(client_opts: ClientOptions, config: Config) -> Result<(), AcpError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(AcpError::Io)?;

    rt.block_on(server::run_server(client_opts, config))
}
