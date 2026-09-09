//! Global request handlers: MCP servers, plugins, models, sessions.

use pacode_types::{Reply, Request};

use super::Core;

impl Core {
    /// Requests that need no session: `ListSessions`, `ListModels`, MCP, and Plugin requests.
    /// Used by the daemon for unattached connections too.
    pub async fn handle_global(&self, req: &Request) -> Option<Reply> {
        match req {
            Request::ListSessions { limit } => Some(
                match self
                    .deps
                    .store
                    .list_sessions(pacode_store::SessionFilter {
                        cwd: None,
                        limit: *limit,
                    })
                    .await
                {
                    Ok(sessions) => Reply::Sessions { sessions },
                    Err(e) => Reply::Error {
                        message: e.to_string(),
                    },
                },
            ),
            Request::ListModels => Some(Reply::Models {
                models: self.providers().list_all_models().await,
            }),
            Request::ListMcpServers => {
                let statuses = self.deps.mcp.statuses();
                let mut servers = Vec::new();
                for (name, status) in statuses {
                    let is_enabled = self.deps.mcp.is_enabled(&name);
                    let (status_str, err_str, tools, resources, prompts, prompt_names) =
                        match status {
                            pacode_mcp::ServerStatus::Stopped => {
                                let s = if is_enabled { "stopped" } else { "disabled" };
                                (s.to_string(), None, 0, 0, 0, Vec::new())
                            }
                            pacode_mcp::ServerStatus::Starting => {
                                let s = if is_enabled { "starting" } else { "disabled" };
                                (s.to_string(), None, 0, 0, 0, Vec::new())
                            }
                            pacode_mcp::ServerStatus::Ready {
                                tools,
                                resources,
                                prompts,
                            } => {
                                let prompt_names = self
                                    .deps
                                    .mcp
                                    .list_prompts(&name)
                                    .await
                                    .map(|ps| ps.into_iter().map(|p| p.name).collect())
                                    .unwrap_or_default();
                                (
                                    "ready".to_string(),
                                    None,
                                    tools as u32,
                                    resources as u32,
                                    prompts as u32,
                                    prompt_names,
                                )
                            }
                            pacode_mcp::ServerStatus::Failed(err) => {
                                let s = if is_enabled { "failed" } else { "disabled" };
                                (s.to_string(), Some(err), 0, 0, 0, Vec::new())
                            }
                        };
                    servers.push(pacode_types::protocol::McpServerInfo {
                        name,
                        status: status_str,
                        error: err_str,
                        tools,
                        resources,
                        prompts,
                        prompt_names,
                    });
                }
                Some(Reply::McpServers { servers })
            }
            Request::RestartMcpServer { server } => {
                if let Ok(mut guard) = self.cached_mcp_tools.lock() {
                    *guard = None;
                }
                match self.deps.mcp.restart(server).await {
                    Ok(()) => Some(Reply::Ok),
                    Err(e) => Some(Reply::Error {
                        message: e.to_string(),
                    }),
                }
            }
            Request::SetMcpServerEnabled { server, enabled } => {
                match self.deps.mcp.set_enabled(server, *enabled).await {
                    Ok(()) => {
                        let fresh_mcp_tools =
                            pacode_tools::builtin::mcp::mcp_tools(self.deps.mcp.clone()).await;
                        if let Ok(mut guard) = self.cached_mcp_tools.lock() {
                            *guard = Some(fresh_mcp_tools.clone());
                        }
                        let mut new_tools = self.deps.tools.clone();
                        for tool in &fresh_mcp_tools {
                            new_tools.register(tool.clone());
                        }
                        if let Ok(sessions) = self.sessions.read() {
                            for session in sessions.values() {
                                if let Ok(mut g) = session.tools.write() {
                                    *g = new_tools.clone();
                                }
                                if let Some(main) = session.main_agent()
                                    && let Ok(mut g) = main.tools.write()
                                {
                                    *g = new_tools.clone();
                                }
                            }
                        }
                        Some(Reply::Ok)
                    }
                    Err(e) => Some(Reply::Error {
                        message: e.to_string(),
                    }),
                }
            }
            Request::GetMcpPrompt { server, name, args } => {
                let val_args = serde_json::to_value(args).unwrap_or(serde_json::Value::Null);
                match self.deps.mcp.get_prompt(server, name, val_args).await {
                    Ok(text) => Some(Reply::McpPrompt { text }),
                    Err(e) => Some(Reply::Error {
                        message: e.to_string(),
                    }),
                }
            }
            Request::ListPlugins => {
                let plugins = self
                    .deps
                    .plugins
                    .list()
                    .into_iter()
                    .map(|p| pacode_types::protocol::PluginInfo {
                        name: p.name,
                        version: p.version,
                        kind: p.kind.map(|k| k.to_string()).unwrap_or_default(),
                        tools: p.tools.into_iter().map(|t| t.name).collect(),
                        commands: p.commands.into_iter().map(|c| c.name).collect(),
                        error: p.error,
                    })
                    .collect();
                Some(Reply::Plugins { plugins })
            }
            Request::RunPluginCommand { name, args } => {
                match self.deps.plugins.run_command(name, args.clone()).await {
                    Ok(outcome) => {
                        let proto_outcome = match outcome {
                            pacode_plugin::CommandOutcome::InsertText(text) => {
                                pacode_types::protocol::PluginCommandOutcome::InsertText { text }
                            }
                            pacode_plugin::CommandOutcome::SendPrompt(text) => {
                                pacode_types::protocol::PluginCommandOutcome::SendPrompt { text }
                            }
                            pacode_plugin::CommandOutcome::Nothing => {
                                pacode_types::protocol::PluginCommandOutcome::Nothing
                            }
                        };
                        Some(Reply::PluginCommand(proto_outcome))
                    }
                    Err(e) => Some(Reply::Error {
                        message: e.to_string(),
                    }),
                }
            }
            _ => None,
        }
    }
}
