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
            Request::BrowseMarketplace { source, query } => {
                Some(self.browse_marketplace(source, query).await)
            }
            Request::InstallPlugin { source, name } => {
                Some(self.install_plugin(source, name).await)
            }
            Request::UninstallPlugin { name } => Some(self.uninstall_plugin(name).await),
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

impl Core {
    /// Plugins a marketplace offers, with their installed state.
    async fn browse_marketplace(&self, source: &str, query: &str) -> Reply {
        let source = match pacode_plugin::marketplace::MarketplaceSource::parse(source) {
            Ok(s) => s,
            Err(e) => {
                return Reply::Error {
                    message: e.to_string(),
                };
            }
        };
        match self
            .deps
            .marketplace
            .list(&source, query, pacode_types::now_ms())
            .await
        {
            Ok(listings) => {
                // Whether the listing is stale is a property of the index, so it
                // is read once rather than per plugin.
                let stale = self
                    .deps
                    .marketplace
                    .index(&source, pacode_types::now_ms())
                    .await
                    .map(|i| i.stale)
                    .unwrap_or(false);
                Reply::MarketplacePlugins {
                    plugins: listings.iter().map(to_info).collect(),
                    stale,
                }
            }
            Err(e) => Reply::Error {
                message: e.to_string(),
            },
        }
    }

    async fn install_plugin(&self, source: &str, name: &str) -> Reply {
        let parsed = match pacode_plugin::marketplace::MarketplaceSource::parse(source) {
            Ok(s) => s,
            Err(e) => {
                return Reply::Error {
                    message: e.to_string(),
                };
            }
        };
        match self
            .deps
            .marketplace
            .install(&parsed, name, pacode_types::now_ms())
            .await
        {
            Ok(record) => {
                log::info!("installed plugin {} from {}", record.name, record.source);
                self.browse_marketplace(source, "").await
            }
            Err(e) => Reply::Error {
                message: e.to_string(),
            },
        }
    }

    async fn uninstall_plugin(&self, name: &str) -> Reply {
        match self.deps.marketplace.uninstall(name) {
            Ok(()) => Reply::MarketplacePlugins {
                plugins: self
                    .deps
                    .marketplace
                    .installed()
                    .into_iter()
                    .map(|installed| pacode_types::MarketplacePluginInfo {
                        name: installed.name,
                        description: String::new(),
                        version: installed.version.clone(),
                        author: String::new(),
                        category: String::new(),
                        marketplace: installed.source,
                        installed: true,
                        installed_version: Some(installed.version),
                        update_available: false,
                    })
                    .collect(),
                stale: false,
            },
            Err(e) => Reply::Error {
                message: e.to_string(),
            },
        }
    }
}

fn to_info(
    listing: &pacode_plugin::marketplace::PluginListing,
) -> pacode_types::MarketplacePluginInfo {
    pacode_types::MarketplacePluginInfo {
        name: listing.entry.name.clone(),
        description: listing.entry.description.clone(),
        version: listing.entry.version.clone(),
        author: listing.entry.author.display(),
        category: listing.entry.category.clone(),
        marketplace: listing.marketplace.clone(),
        installed: listing.installed.is_some(),
        installed_version: listing.installed.as_ref().map(|i| i.version.clone()),
        update_available: listing.update_available(),
    }
}
