//! `pacode plugins`: manage plugins.

use std::io::Write;
use std::sync::Arc;

use anyhow::Context;
use pacode_plugin::PluginHost;
use pacode_plugin::sink::NoopUiSink;
use pacode_plugin::types::PluginKind;
use pacode_types::Config;

use crate::cli::PluginsAction;

#[cfg(test)]
mod plugins_cmd_tests;

pub fn run(action: Option<PluginsAction>, config: &Config) -> anyhow::Result<()> {
    match action.unwrap_or(PluginsAction::List) {
        PluginsAction::List => run_list(config),
    }
}

pub fn run_list(config: &Config) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    let has_errors = rt.block_on(async { list_plugins(config, &mut std::io::stdout()).await })?;
    if has_errors {
        anyhow::bail!("one or more plugins failed to load");
    }
    Ok(())
}

pub async fn list_plugins(config: &Config, out: &mut impl Write) -> anyhow::Result<bool> {
    let host = PluginHost::load(&config.plugins, Arc::new(NoopUiSink)).await;
    let list = host.list();

    writeln!(
        out,
        "{:<16}  {:<9}  {:<6}  {:<20}  {:<20}  ERROR",
        "NAME", "VERSION", "KIND", "TOOLS", "COMMANDS"
    )?;

    let mut has_errors = !host.errors.is_empty();

    for p in &list {
        if p.error.is_some() {
            has_errors = true;
        }
        let ver = if p.version.is_empty() {
            "-"
        } else {
            &p.version
        };
        let kind = match p.kind {
            Some(PluginKind::Lua) => "lua",
            Some(PluginKind::Wasm) => "wasm",
            None => "-",
        };
        let tools = if p.tools.is_empty() {
            "-".to_string()
        } else {
            p.tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let cmds = if p.commands.is_empty() {
            "-".to_string()
        } else {
            p.commands
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let err = p.error.as_deref().unwrap_or("-");

        writeln!(
            out,
            "{:<16}  {:<9}  {:<6}  {:<20}  {:<20}  {}",
            p.name, ver, kind, tools, cmds, err
        )?;
    }

    Ok(has_errors)
}
