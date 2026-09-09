//! `pacode mcp`: manage Model Context Protocol servers in config.toml.

use std::io::Write;

use anyhow::{Context, bail};
use pacode_config::Paths;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::cli::McpAction;

#[cfg(test)]
mod mcp_cmd_tests;

#[derive(Debug, Default)]
pub struct AddArgs {
    pub name: String,
    pub cmd: Option<String>,
    pub url: Option<String>,
    pub headers: Vec<String>,
    pub env: Vec<String>,
    pub lazy: bool,
    pub no_lazy: bool,
}

pub fn run(action: Option<McpAction>, paths: &Paths) -> anyhow::Result<()> {
    match action.unwrap_or(McpAction::List) {
        McpAction::List => list(paths, &mut std::io::stdout()),
        McpAction::Add {
            name,
            cmd,
            url,
            headers,
            env,
            lazy,
            no_lazy,
        } => {
            let args = AddArgs {
                name,
                cmd,
                url,
                headers,
                env,
                lazy,
                no_lazy,
            };
            add(args, paths, &mut std::io::stdout())
        }
        McpAction::Remove { name } => remove(&name, paths, &mut std::io::stdout()),
    }
}

pub fn list(paths: &Paths, out: &mut impl Write) -> anyhow::Result<()> {
    let config = pacode_config::load(paths).context("failed to load configuration")?;
    writeln!(
        out,
        "{:<16}  {:<9}  {:<32}  {:<7}  LAZY",
        "NAME", "TRANSPORT", "COMMAND/URL", "ENABLED"
    )?;
    for (name, srv) in &config.mcp.servers {
        let (transport, target) = if let Some(url) = &srv.url
            && !url.trim().is_empty()
        {
            ("http", url.clone())
        } else {
            let cmd = if srv.args.is_empty() {
                srv.command.clone()
            } else {
                format!("{} {}", srv.command, srv.args.join(" "))
            };
            ("stdio", cmd)
        };
        writeln!(
            out,
            "{:<16}  {:<9}  {:<32}  {:<7}  {}",
            name, transport, target, srv.enabled, srv.lazy
        )?;
    }
    Ok(())
}

pub fn add(args: AddArgs, paths: &Paths, out: &mut impl Write) -> anyhow::Result<()> {
    if args.cmd.is_none() && args.url.is_none() {
        bail!("either --cmd or --url must be specified");
    }
    if args.cmd.is_some() && args.url.is_some() {
        bail!("--cmd and --url are mutually exclusive");
    }

    let mut parsed_headers = Vec::new();
    for h in &args.headers {
        let (k, v) = parse_key_val(h).context("invalid --header")?;
        parsed_headers.push((k, v));
    }

    let mut parsed_env = Vec::new();
    for e in &args.env {
        let (k, v) = parse_key_val(e).context("invalid --env")?;
        parsed_env.push((k, v));
    }

    let config_path = paths.config_file();
    let mut doc = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        content
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse {}", config_path.display()))?
    } else {
        DocumentMut::new()
    };

    if !doc.contains_key("mcp") {
        let mut t = Table::new();
        t.set_implicit(true);
        doc["mcp"] = Item::Table(t);
    }
    let mcp = doc["mcp"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[mcp] in config is not a table"))?;

    if !mcp.contains_key("servers") {
        let mut t = Table::new();
        t.set_implicit(true);
        mcp["servers"] = Item::Table(t);
    }
    let servers = mcp["servers"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[mcp.servers] in config is not a table"))?;

    let is_update = servers.contains_key(&args.name);
    if !is_update {
        servers.insert(&args.name, Item::Table(Table::new()));
    }
    let srv = servers[&args.name]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[mcp.servers.{}] is not a table", args.name))?;

    if let Some(ref cmd_str) = args.cmd {
        let parts =
            shlex::split(cmd_str).ok_or_else(|| anyhow::anyhow!("failed to parse --cmd"))?;
        if parts.is_empty() {
            bail!("--cmd cannot be empty");
        }
        let command = &parts[0];
        let cmd_args = &parts[1..];

        srv.remove("url");
        srv.remove("headers");
        srv.insert("command", value(command.clone()));
        let mut arr = Array::new();
        for a in cmd_args {
            arr.push(a.as_str());
        }
        srv.insert("args", Item::Value(Value::Array(arr)));
        if !parsed_env.is_empty() {
            let mut env_tbl = InlineTable::new();
            for (k, v) in parsed_env {
                env_tbl.insert(k, Value::from(v));
            }
            srv.insert("env", Item::Value(Value::InlineTable(env_tbl)));
        }
    } else if let Some(ref url_str) = args.url {
        srv.remove("command");
        srv.remove("args");
        srv.remove("env");
        srv.insert("url", value(url_str.clone()));
        if !parsed_headers.is_empty() {
            let mut hdr_tbl = InlineTable::new();
            for (k, v) in parsed_headers {
                hdr_tbl.insert(k, Value::from(v));
            }
            srv.insert("headers", Item::Value(Value::InlineTable(hdr_tbl)));
        }
    }

    let lazy_val = if args.no_lazy {
        false
    } else if args.lazy {
        true
    } else {
        srv.get("lazy").and_then(|v| v.as_bool()).unwrap_or(true)
    };
    srv.insert("lazy", value(lazy_val));

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(&config_path, doc.to_string())
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    let verb = if is_update { "Updated" } else { "Added" };
    writeln!(out, "{verb} MCP server '{}'", args.name)?;
    Ok(())
}

pub fn remove(name: &str, paths: &Paths, out: &mut impl Write) -> anyhow::Result<()> {
    let config_path = paths.config_file();
    if !config_path.exists() {
        bail!("MCP server '{name}' not found");
    }
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let mut doc: DocumentMut = content
        .parse::<DocumentMut>()
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    let removed = doc
        .get_mut("mcp")
        .and_then(|m| m.as_table_mut())
        .and_then(|m| m.get_mut("servers"))
        .and_then(|s| s.as_table_mut())
        .and_then(|s| s.remove(name));

    if removed.is_none() {
        bail!("MCP server '{name}' not found");
    }

    std::fs::write(&config_path, doc.to_string())
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    writeln!(out, "Removed MCP server '{name}'")?;
    Ok(())
}

fn parse_key_val(s: &str) -> anyhow::Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("invalid key-value pair '{s}', expected K=V"))?;
    let k = k.trim();
    if k.is_empty() {
        bail!("empty key in key-value pair '{s}'");
    }
    Ok((k.to_string(), v.to_string()))
}
