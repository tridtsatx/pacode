use std::fs;

use pacode_config::Paths;
use tempfile::tempdir;

use super::*;

#[test]
fn test_mcp_add_stdio() {
    let dir = tempdir().unwrap();
    let paths = Paths::under(dir.path());

    let mut out = Vec::new();
    let args = AddArgs {
        name: "fs".to_string(),
        cmd: Some("npx -y @modelcontextprotocol/server-filesystem .".to_string()),
        url: None,
        headers: vec![],
        env: vec!["DIR=/workspace".to_string()],
        lazy: true,
        no_lazy: false,
    };

    add(args, &paths, &mut out).unwrap();
    let out_str = String::from_utf8(out).unwrap();
    assert!(out_str.contains("Added MCP server 'fs'"));

    let content = fs::read_to_string(paths.config_file()).unwrap();
    assert!(content.contains("[mcp.servers.fs]"));
    assert!(content.contains("command = \"npx\""));
    assert!(content.contains(r#"args = ["-y", "@modelcontextprotocol/server-filesystem", "."]"#));
    assert!(content.contains("lazy = true"));
    assert!(content.contains("DIR = \"/workspace\""));
}

#[test]
fn test_mcp_add_http_with_headers() {
    let dir = tempdir().unwrap();
    let paths = Paths::under(dir.path());

    let mut out = Vec::new();
    let args = AddArgs {
        name: "remote".to_string(),
        cmd: None,
        url: Some("http://127.0.0.1:8080/mcp".to_string()),
        headers: vec![
            "Authorization=Bearer token123".to_string(),
            "X-App=pacode".to_string(),
        ],
        env: vec![],
        lazy: false,
        no_lazy: true,
    };

    add(args, &paths, &mut out).unwrap();
    let out_str = String::from_utf8(out).unwrap();
    assert!(out_str.contains("Added MCP server 'remote'"));

    let content = fs::read_to_string(paths.config_file()).unwrap();
    assert!(content.contains("[mcp.servers.remote]"));
    assert!(content.contains(r#"url = "http://127.0.0.1:8080/mcp""#));
    assert!(content.contains("Authorization = \"Bearer token123\""));
    assert!(content.contains("X-App = \"pacode\""));
    assert!(content.contains("lazy = false"));
}

#[test]
fn test_mcp_update_existing_and_preserve_comments() {
    let dir = tempdir().unwrap();
    let paths = Paths::under(dir.path());

    let initial = r#"# Top-level comment
[provider]
default = "bubna/gemini"

# MCP comment
[mcp.servers.srv]
command = "old_cmd"
args = ["arg1"]
lazy = true
"#;
    fs::write(paths.config_file(), initial).unwrap();

    let mut out = Vec::new();
    let args = AddArgs {
        name: "srv".to_string(),
        cmd: Some("new_cmd --flag 1".to_string()),
        url: None,
        headers: vec![],
        env: vec![],
        lazy: false,
        no_lazy: true,
    };

    add(args, &paths, &mut out).unwrap();
    let out_str = String::from_utf8(out).unwrap();
    assert!(out_str.contains("Updated MCP server 'srv'"));

    let content = fs::read_to_string(paths.config_file()).unwrap();
    assert!(content.contains("# Top-level comment"));
    assert!(content.contains("[provider]"));
    assert!(content.contains("default = \"bubna/gemini\""));
    assert!(content.contains("# MCP comment"));
    assert!(content.contains("command = \"new_cmd\""));
    assert!(content.contains(r#"args = ["--flag", "1"]"#));
    assert!(content.contains("lazy = false"));
}

#[test]
fn test_mcp_remove() {
    let dir = tempdir().unwrap();
    let paths = Paths::under(dir.path());

    let initial = r#"[mcp.servers.keep_me]
url = "http://example.com"

[mcp.servers.delete_me]
command = "test"
args = []
"#;
    fs::write(paths.config_file(), initial).unwrap();

    let mut out = Vec::new();
    remove("delete_me", &paths, &mut out).unwrap();
    let out_str = String::from_utf8(out).unwrap();
    assert!(out_str.contains("Removed MCP server 'delete_me'"));

    let content = fs::read_to_string(paths.config_file()).unwrap();
    assert!(content.contains("keep_me"));
    assert!(!content.contains("delete_me"));

    // Removing non-existent server fails
    let mut err_out = Vec::new();
    let err = remove("nonexistent", &paths, &mut err_out).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn test_mcp_list_output() {
    let dir = tempdir().unwrap();
    let paths = Paths::under(dir.path());

    let toml_content = r#"[mcp.servers.local_tool]
command = "python3"
args = ["server.py", "--port", "9000"]
lazy = true
enabled = true

[mcp.servers.remote_tool]
url = "https://mcp.example.com/sse"
lazy = false
enabled = false
"#;
    fs::write(paths.config_file(), toml_content).unwrap();

    let mut out = Vec::new();
    list(&paths, &mut out).unwrap();
    let out_str = String::from_utf8(out).unwrap();

    assert!(out_str.contains("NAME"));
    assert!(out_str.contains("TRANSPORT"));
    assert!(out_str.contains("COMMAND/URL"));
    assert!(out_str.contains("ENABLED"));
    assert!(out_str.contains("LAZY"));

    assert!(out_str.contains("local_tool"));
    assert!(out_str.contains("stdio"));
    assert!(out_str.contains("python3 server.py --port 9000"));
    assert!(out_str.contains("true"));

    assert!(out_str.contains("remote_tool"));
    assert!(out_str.contains("http"));
    assert!(out_str.contains("https://mcp.example.com/sse"));
    assert!(out_str.contains("false"));
}

#[test]
fn test_mcp_validation_errors() {
    let dir = tempdir().unwrap();
    let paths = Paths::under(dir.path());

    // Neither cmd nor url
    let mut out = Vec::new();
    let args = AddArgs {
        name: "invalid".to_string(),
        cmd: None,
        url: None,
        ..Default::default()
    };
    assert!(add(args, &paths, &mut out).is_err());

    // Both cmd and url
    let args = AddArgs {
        name: "invalid".to_string(),
        cmd: Some("cmd".to_string()),
        url: Some("http://url".to_string()),
        ..Default::default()
    };
    assert!(add(args, &paths, &mut out).is_err());

    // Invalid header
    let args = AddArgs {
        name: "invalid".to_string(),
        url: Some("http://url".to_string()),
        headers: vec!["NoEqualsSign".to_string()],
        ..Default::default()
    };
    assert!(add(args, &paths, &mut out).is_err());
}
