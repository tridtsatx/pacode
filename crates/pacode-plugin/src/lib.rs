//! Plugin execution runtime and host for pacode.

pub mod error;
pub mod host;
pub mod lua;
pub mod runtime;
pub mod sink;
pub mod types;
pub mod wasm;

pub use error::PluginError;
pub use host::PluginHost;
pub use lua::LuaPlugin;
pub use runtime::PluginRuntime;
pub use sink::{NoopUiSink, UiSink};
pub use types::{
    CommandOutcome, HookEvent, HookResult, PluginCommandDef, PluginInfo, PluginKind,
    PluginManifest, PluginToolDef,
};
pub use wasm::WasmPlugin;
