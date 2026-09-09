//! Object-safe runtime trait for plugin execution.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::PluginError;
use crate::types::{
    CommandOutcome, HookEvent, HookResult, PluginCommandDef, PluginKind, PluginToolDef,
};

#[async_trait]
pub trait PluginRuntime: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn kind(&self) -> PluginKind;
    fn description(&self) -> Option<&str>;
    fn tools(&self) -> Vec<PluginToolDef>;
    fn commands(&self) -> Vec<PluginCommandDef>;
    async fn call_tool(&self, name: &str, input: Value) -> Result<Value, PluginError>;
    async fn run_command(&self, name: &str, args: String) -> Result<CommandOutcome, PluginError>;
    async fn hook(&self, event: HookEvent) -> Result<HookResult, PluginError>;
}
