//! Per-source MCP server discovery and normalization.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::{DiscoveredMcp, Discovery, ImportSource, ImportWarning, McpServerConfig};

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;

/// Discovers MCP servers for a given import source.
pub fn discover_mcp(home: &Path, cwd: &Path, source: ImportSource, discovery: &mut Discovery) {
    match source {
        ImportSource::ClaudeCode => {
            let user_path = home.join(".claude.json");
            parse_claude_like_json(
                &user_path,
                "mcpServers",
                ImportSource::ClaudeCode,
                discovery,
            );
            let project_path = cwd.join(".mcp.json");
            parse_claude_like_json(
                &project_path,
                "mcpServers",
                ImportSource::ClaudeCode,
                discovery,
            );
        }
        ImportSource::Codex => {
            discover_codex_mcp(home, discovery);
        }
        ImportSource::OpenCode => {
            let path = home.join(".config").join("opencode").join("opencode.json");
            parse_opencode_json(&path, discovery);
        }
        ImportSource::Cursor => {
            let path = home.join(".cursor").join("mcp.json");
            parse_claude_like_json(&path, "mcpServers", ImportSource::Cursor, discovery);
        }
        ImportSource::GeminiCli => {
            let path = home.join(".gemini").join("settings.json");
            parse_gemini_json(&path, discovery);
        }
        ImportSource::VsCode => {
            let path = cwd.join(".vscode").join("mcp.json");
            parse_vscode_json(&path, discovery);
        }
    }
}

struct RawServer {
    is_remote: bool,
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    url: Option<String>,
    headers: BTreeMap<String, String>,
    enabled: bool,
}

fn build_server_config(name: &str, raw: RawServer) -> Result<McpServerConfig, String> {
    if raw.is_remote {
        let Some(u) = raw.url else {
            return Err(format!("remote server '{name}' missing 'url'"));
        };
        Ok(McpServerConfig {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: Some(u),
            headers: raw.headers,
            enabled: raw.enabled,
            lazy: true,
            timeout_secs: 60,
        })
    } else if let Some(cmd) = raw.command {
        Ok(McpServerConfig {
            command: cmd,
            args: raw.args,
            env: raw.env,
            url: None,
            headers: BTreeMap::new(),
            enabled: raw.enabled,
            lazy: true,
            timeout_secs: 60,
        })
    } else {
        Err(format!("server '{name}' has neither 'command' nor 'url'"))
    }
}

fn parse_json_file(
    path: &Path,
    source: ImportSource,
    discovery: &mut Discovery,
) -> Option<serde_json::Map<String, Value>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: format!("failed to read file: {err}"),
            });
            return None;
        }
    };

    let root: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(err) => {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: format!("failed to parse JSON: {err}"),
            });
            return None;
        }
    };

    match root {
        Value::Object(map) => Some(map),
        _ => {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: "expected JSON object at root".to_string(),
            });
            None
        }
    }
}

fn json_value_to_string_map(val: &Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Value::Object(obj) = val {
        for (k, v) in obj {
            let val_str = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            map.insert(k.clone(), val_str);
        }
    }
    map
}

fn json_value_to_string_vec(val: &Value) -> Vec<String> {
    match val {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            })
            .collect(),
        Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn parse_claude_like_json(
    path: &Path,
    servers_key: &str,
    source: ImportSource,
    discovery: &mut Discovery,
) {
    let Some(root) = parse_json_file(path, source, discovery) else {
        return;
    };
    let Some(servers_val) = root.get(servers_key) else {
        return;
    };
    let Some(servers_obj) = servers_val.as_object() else {
        discovery.errors.push(ImportWarning {
            source,
            path: path.to_path_buf(),
            message: format!("'{servers_key}' is not a JSON object"),
        });
        return;
    };

    for (name, entry_val) in servers_obj {
        let Some(entry) = entry_val.as_object() else {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: format!("server '{name}' is not an object"),
            });
            continue;
        };

        let server_type = entry.get("type").and_then(|v| v.as_str());
        let url = entry.get("url").and_then(|v| v.as_str()).map(String::from);
        let headers = entry
            .get("headers")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let command = entry
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let args = entry
            .get("args")
            .map(json_value_to_string_vec)
            .unwrap_or_default();
        let env = entry
            .get("env")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let is_remote = server_type == Some("http")
            || server_type == Some("sse")
            || server_type == Some("remote")
            || url.is_some();

        match build_server_config(
            name,
            RawServer {
                is_remote,
                command,
                args,
                env,
                url,
                headers,
                enabled,
            },
        ) {
            Ok(server) => discovery.mcp.push(DiscoveredMcp {
                source,
                origin: path.to_path_buf(),
                name: name.clone(),
                server,
            }),
            Err(msg) => discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: msg,
            }),
        }
    }
}

fn parse_opencode_json(path: &Path, discovery: &mut Discovery) {
    let source = ImportSource::OpenCode;
    let Some(root) = parse_json_file(path, source, discovery) else {
        return;
    };
    let Some(servers_val) = root.get("mcp") else {
        return;
    };
    let Some(servers_obj) = servers_val.as_object() else {
        discovery.errors.push(ImportWarning {
            source,
            path: path.to_path_buf(),
            message: "'mcp' is not a JSON object".to_string(),
        });
        return;
    };

    for (name, entry_val) in servers_obj {
        let Some(entry) = entry_val.as_object() else {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: format!("server '{name}' is not an object"),
            });
            continue;
        };

        let server_type = entry.get("type").and_then(|v| v.as_str());
        let url = entry.get("url").and_then(|v| v.as_str()).map(String::from);
        let headers = entry
            .get("headers")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let env = entry
            .get("environment")
            .or_else(|| entry.get("env"))
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let is_remote = server_type == Some("remote")
            || server_type == Some("http")
            || server_type == Some("sse")
            || url.is_some();

        let (command, args) = if is_remote {
            (None, Vec::new())
        } else {
            match entry.get("command") {
                Some(Value::Array(arr)) => {
                    if arr.is_empty() {
                        discovery.errors.push(ImportWarning {
                            source,
                            path: path.to_path_buf(),
                            message: format!("server '{name}' has empty 'command' array"),
                        });
                        continue;
                    }
                    let head = match &arr[0] {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let tail: Vec<String> = arr[1..]
                        .iter()
                        .filter_map(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            Value::Number(n) => Some(n.to_string()),
                            Value::Bool(b) => Some(b.to_string()),
                            _ => None,
                        })
                        .collect();
                    (Some(head), tail)
                }
                Some(Value::String(s)) => {
                    let extra = entry
                        .get("args")
                        .map(json_value_to_string_vec)
                        .unwrap_or_default();
                    (Some(s.clone()), extra)
                }
                _ => (None, Vec::new()),
            }
        };

        match build_server_config(
            name,
            RawServer {
                is_remote,
                command,
                args,
                env,
                url,
                headers,
                enabled,
            },
        ) {
            Ok(server) => discovery.mcp.push(DiscoveredMcp {
                source,
                origin: path.to_path_buf(),
                name: name.clone(),
                server,
            }),
            Err(msg) => discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: msg,
            }),
        }
    }
}

fn parse_gemini_json(path: &Path, discovery: &mut Discovery) {
    let source = ImportSource::GeminiCli;
    let Some(root) = parse_json_file(path, source, discovery) else {
        return;
    };
    let Some(servers_val) = root.get("mcpServers") else {
        return;
    };
    let Some(servers_obj) = servers_val.as_object() else {
        discovery.errors.push(ImportWarning {
            source,
            path: path.to_path_buf(),
            message: "'mcpServers' is not a JSON object".to_string(),
        });
        return;
    };

    for (name, entry_val) in servers_obj {
        let Some(entry) = entry_val.as_object() else {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: format!("server '{name}' is not an object"),
            });
            continue;
        };

        let http_url = entry.get("httpUrl").and_then(|v| v.as_str());
        let sse_url = entry.get("url").and_then(|v| v.as_str());
        let url = http_url.or(sse_url).map(String::from);
        let headers = entry
            .get("headers")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let command = entry
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let args = entry
            .get("args")
            .map(json_value_to_string_vec)
            .unwrap_or_default();
        let env = entry
            .get("env")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let is_remote = url.is_some();

        match build_server_config(
            name,
            RawServer {
                is_remote,
                command,
                args,
                env,
                url,
                headers,
                enabled,
            },
        ) {
            Ok(server) => discovery.mcp.push(DiscoveredMcp {
                source,
                origin: path.to_path_buf(),
                name: name.clone(),
                server,
            }),
            Err(msg) => discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: msg,
            }),
        }
    }
}

fn parse_vscode_json(path: &Path, discovery: &mut Discovery) {
    let source = ImportSource::VsCode;
    let Some(root) = parse_json_file(path, source, discovery) else {
        return;
    };
    let Some(servers_val) = root.get("servers") else {
        return;
    };
    let Some(servers_obj) = servers_val.as_object() else {
        discovery.errors.push(ImportWarning {
            source,
            path: path.to_path_buf(),
            message: "'servers' is not a JSON object".to_string(),
        });
        return;
    };

    for (name, entry_val) in servers_obj {
        let Some(entry) = entry_val.as_object() else {
            discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: format!("server '{name}' is not an object"),
            });
            continue;
        };

        let server_type = entry.get("type").and_then(|v| v.as_str());
        let url = entry.get("url").and_then(|v| v.as_str()).map(String::from);
        let headers = entry
            .get("headers")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let command = entry
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let args = entry
            .get("args")
            .map(json_value_to_string_vec)
            .unwrap_or_default();
        let env = entry
            .get("env")
            .map(json_value_to_string_map)
            .unwrap_or_default();
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let is_remote = server_type == Some("http") || server_type == Some("sse") || url.is_some();

        match build_server_config(
            name,
            RawServer {
                is_remote,
                command,
                args,
                env,
                url,
                headers,
                enabled,
            },
        ) {
            Ok(server) => discovery.mcp.push(DiscoveredMcp {
                source,
                origin: path.to_path_buf(),
                name: name.clone(),
                server,
            }),
            Err(msg) => discovery.errors.push(ImportWarning {
                source,
                path: path.to_path_buf(),
                message: msg,
            }),
        }
    }
}

fn discover_codex_mcp(home: &Path, discovery: &mut Discovery) {
    let source = ImportSource::Codex;
    let path = home.join(".codex").join("config.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            discovery.errors.push(ImportWarning {
                source,
                path,
                message: format!("failed to read file: {err}"),
            });
            return;
        }
    };

    let table: toml::Table = match toml::from_str(&content) {
        Ok(t) => t,
        Err(err) => {
            discovery.errors.push(ImportWarning {
                source,
                path,
                message: format!("failed to parse TOML: {err}"),
            });
            return;
        }
    };

    let Some(mcp_servers_val) = table
        .get("mcp_servers")
        .or_else(|| table.get("mcp-servers"))
    else {
        return;
    };

    let Some(mcp_servers_tbl) = mcp_servers_val.as_table() else {
        discovery.errors.push(ImportWarning {
            source,
            path,
            message: "'mcp_servers' is not a TOML table".to_string(),
        });
        return;
    };

    for (name, server_val) in mcp_servers_tbl {
        let Some(server_tbl) = server_val.as_table() else {
            discovery.errors.push(ImportWarning {
                source,
                path: path.clone(),
                message: format!("server '{name}' is not a TOML table"),
            });
            continue;
        };

        let url = server_tbl
            .get("url")
            .and_then(|v| v.as_str())
            .map(String::from);
        let headers = server_tbl
            .get("http_headers")
            .or_else(|| server_tbl.get("headers"))
            .and_then(|v| v.as_table())
            .map(toml_table_to_string_map)
            .unwrap_or_default();
        let command = server_tbl
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let args = server_tbl
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| match x {
                        toml::Value::String(s) => Some(s.clone()),
                        toml::Value::Integer(i) => Some(i.to_string()),
                        toml::Value::Boolean(b) => Some(b.to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let env = server_tbl
            .get("env")
            .and_then(|v| v.as_table())
            .map(toml_table_to_string_map)
            .unwrap_or_default();
        let enabled = server_tbl
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let is_remote = url.is_some();

        match build_server_config(
            name,
            RawServer {
                is_remote,
                command,
                args,
                env,
                url,
                headers,
                enabled,
            },
        ) {
            Ok(server) => discovery.mcp.push(DiscoveredMcp {
                source,
                origin: path.clone(),
                name: name.clone(),
                server,
            }),
            Err(msg) => discovery.errors.push(ImportWarning {
                source,
                path: path.clone(),
                message: msg,
            }),
        }
    }
}

fn toml_table_to_string_map(table: &toml::Table) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (k, v) in table {
        let val_str = match v {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Float(f) => f.to_string(),
            _ => continue,
        };
        map.insert(k.clone(), val_str);
    }
    map
}
