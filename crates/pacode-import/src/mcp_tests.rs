use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::discover;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path =
            std::env::temp_dir().join(format!("pacode-import-test-mcp-{prefix}-{pid}-{count}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_import_source_api() {
    assert_eq!(ImportSource::all().len(), 6);
    for source in ImportSource::all() {
        let id = source.id();
        let parsed: ImportSource = id.parse().expect("parse by id");
        assert_eq!(*source, parsed);
        assert!(!source.label().is_empty());
    }

    assert_eq!(
        "claude".parse::<ImportSource>().unwrap(),
        ImportSource::ClaudeCode
    );
    assert_eq!(
        "codex".parse::<ImportSource>().unwrap(),
        ImportSource::Codex
    );
    assert_eq!(
        "opencode".parse::<ImportSource>().unwrap(),
        ImportSource::OpenCode
    );
    assert_eq!(
        "cursor".parse::<ImportSource>().unwrap(),
        ImportSource::Cursor
    );
    assert_eq!(
        "gemini".parse::<ImportSource>().unwrap(),
        ImportSource::GeminiCli
    );
    assert_eq!(
        "vscode".parse::<ImportSource>().unwrap(),
        ImportSource::VsCode
    );
    assert!("unknown-src".parse::<ImportSource>().is_err());
}

#[test]
fn test_claude_code_mcp() {
    let temp = TempDir::new("claude");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let claude_json = r#"{
        "mcpServers": {
            "user-stdio": {
                "command": "npx",
                "args": ["-y", "claude-tool"],
                "env": { "PORT": "3000" }
            },
            "user-remote": {
                "type": "http",
                "url": "https://claude.example.com/mcp",
                "headers": { "Authorization": "Bearer token1" }
            }
        }
    }"#;
    std::fs::write(home.join(".claude.json"), claude_json).unwrap();

    let proj_mcp_json = r#"{
        "mcpServers": {
            "proj-stdio": {
                "command": "python",
                "args": ["script.py"],
                "env": { "ENV": "test" }
            },
            "proj-sse": {
                "type": "sse",
                "url": "https://sse.example.com/events"
            }
        }
    }"#;
    std::fs::write(cwd.join(".mcp.json"), proj_mcp_json).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::ClaudeCode]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.mcp.len(), 4);

    let user_stdio = disc.mcp.iter().find(|s| s.name == "user-stdio").unwrap();
    assert_eq!(user_stdio.source, ImportSource::ClaudeCode);
    assert_eq!(user_stdio.origin, home.join(".claude.json"));
    assert_eq!(user_stdio.server.command, "npx");
    assert_eq!(user_stdio.server.args, vec!["-y", "claude-tool"]);
    assert_eq!(user_stdio.server.env.get("PORT").unwrap(), "3000");
    assert_eq!(user_stdio.server.url, None);
    assert!(user_stdio.server.headers.is_empty());
    assert!(user_stdio.server.enabled);
    assert!(user_stdio.server.lazy);
    assert_eq!(user_stdio.server.timeout_secs, 60);

    let user_remote = disc.mcp.iter().find(|s| s.name == "user-remote").unwrap();
    assert_eq!(user_remote.source, ImportSource::ClaudeCode);
    assert_eq!(user_remote.origin, home.join(".claude.json"));
    assert_eq!(user_remote.server.command, "");
    assert!(user_remote.server.args.is_empty());
    assert!(user_remote.server.env.is_empty());
    assert_eq!(
        user_remote.server.url.as_deref(),
        Some("https://claude.example.com/mcp")
    );
    assert_eq!(
        user_remote.server.headers.get("Authorization").unwrap(),
        "Bearer token1"
    );
    assert!(user_remote.server.enabled);

    let proj_stdio = disc.mcp.iter().find(|s| s.name == "proj-stdio").unwrap();
    assert_eq!(proj_stdio.origin, cwd.join(".mcp.json"));
    assert_eq!(proj_stdio.server.command, "python");
    assert_eq!(proj_stdio.server.args, vec!["script.py"]);

    let proj_sse = disc.mcp.iter().find(|s| s.name == "proj-sse").unwrap();
    assert_eq!(proj_sse.origin, cwd.join(".mcp.json"));
    assert_eq!(
        proj_sse.server.url.as_deref(),
        Some("https://sse.example.com/events")
    );
}

#[test]
fn test_codex_mcp() {
    let temp = TempDir::new("codex");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let config_toml = r#"
[mcp_servers.codex-stdio]
command = "node"
args = ["codex.js", "--verbose"]
env = { DEBUG = "true" }

[mcp_servers.codex-remote]
url = "https://codex.dev/sse"
http_headers = { "Authorization" = "Bearer codex-tok" }
"#;
    std::fs::write(codex_dir.join("config.toml"), config_toml).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::Codex]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.mcp.len(), 2);

    let stdio = disc.mcp.iter().find(|s| s.name == "codex-stdio").unwrap();
    assert_eq!(stdio.source, ImportSource::Codex);
    assert_eq!(stdio.origin, codex_dir.join("config.toml"));
    assert_eq!(stdio.server.command, "node");
    assert_eq!(stdio.server.args, vec!["codex.js", "--verbose"]);
    assert_eq!(stdio.server.env.get("DEBUG").unwrap(), "true");
    assert_eq!(stdio.server.url, None);
    assert!(stdio.server.headers.is_empty());
    assert!(stdio.server.enabled);

    let remote = disc.mcp.iter().find(|s| s.name == "codex-remote").unwrap();
    assert_eq!(remote.source, ImportSource::Codex);
    assert_eq!(remote.origin, codex_dir.join("config.toml"));
    assert_eq!(remote.server.command, "");
    assert!(remote.server.args.is_empty());
    assert_eq!(remote.server.url.as_deref(), Some("https://codex.dev/sse"));
    assert_eq!(
        remote.server.headers.get("Authorization").unwrap(),
        "Bearer codex-tok"
    );
}

#[test]
fn test_opencode_mcp() {
    let temp = TempDir::new("opencode");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let opencode_dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let opencode_json = r#"{
        "mcp": {
            "local-pkg": {
                "type": "local",
                "command": ["npx", "-y", "pkg"],
                "environment": { "FOO": "bar" },
                "enabled": false
            },
            "remote-pkg": {
                "type": "remote",
                "url": "https://opencode.example.com",
                "headers": { "X-Auth": "secret" },
                "enabled": true
            }
        }
    }"#;
    std::fs::write(opencode_dir.join("opencode.json"), opencode_json).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::OpenCode]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.mcp.len(), 2);

    let local = disc.mcp.iter().find(|s| s.name == "local-pkg").unwrap();
    assert_eq!(local.source, ImportSource::OpenCode);
    assert_eq!(local.origin, opencode_dir.join("opencode.json"));
    assert_eq!(local.server.command, "npx");
    assert_eq!(local.server.args, vec!["-y", "pkg"]);
    assert_eq!(local.server.env.get("FOO").unwrap(), "bar");
    assert!(!local.server.enabled, "enabled:false must be honored");
    assert_eq!(local.server.url, None);
    assert!(local.server.headers.is_empty());

    let remote = disc.mcp.iter().find(|s| s.name == "remote-pkg").unwrap();
    assert_eq!(remote.source, ImportSource::OpenCode);
    assert_eq!(remote.server.command, "");
    assert!(remote.server.args.is_empty());
    assert_eq!(
        remote.server.url.as_deref(),
        Some("https://opencode.example.com")
    );
    assert_eq!(remote.server.headers.get("X-Auth").unwrap(), "secret");
    assert!(remote.server.enabled);
}

#[test]
fn test_cursor_mcp() {
    let temp = TempDir::new("cursor");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let cursor_dir = home.join(".cursor");
    std::fs::create_dir_all(&cursor_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let mcp_json = r#"{
        "mcpServers": {
            "cursor-server": {
                "command": "cursor-agent",
                "args": ["serve"],
                "env": { "CURSOR_ENV": "1" }
            },
            "cursor-remote": {
                "type": "http",
                "url": "https://cursor.remote/mcp",
                "headers": { "Key": "Val" }
            }
        }
    }"#;
    std::fs::write(cursor_dir.join("mcp.json"), mcp_json).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::Cursor]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.mcp.len(), 2);

    let stdio = disc.mcp.iter().find(|s| s.name == "cursor-server").unwrap();
    assert_eq!(stdio.source, ImportSource::Cursor);
    assert_eq!(stdio.origin, cursor_dir.join("mcp.json"));
    assert_eq!(stdio.server.command, "cursor-agent");
    assert_eq!(stdio.server.args, vec!["serve"]);
    assert_eq!(stdio.server.env.get("CURSOR_ENV").unwrap(), "1");
    assert!(stdio.server.enabled);

    let remote = disc.mcp.iter().find(|s| s.name == "cursor-remote").unwrap();
    assert_eq!(remote.source, ImportSource::Cursor);
    assert_eq!(
        remote.server.url.as_deref(),
        Some("https://cursor.remote/mcp")
    );
    assert_eq!(remote.server.headers.get("Key").unwrap(), "Val");
}

#[test]
fn test_gemini_mcp() {
    let temp = TempDir::new("gemini");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let gemini_dir = home.join(".gemini");
    std::fs::create_dir_all(&gemini_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let settings_json = r#"{
        "mcpServers": {
            "gemini-stdio": {
                "command": "python3",
                "args": ["mcp_gemini.py"],
                "env": { "MODEL": "flash" }
            },
            "gemini-http": {
                "httpUrl": "https://gemini.googleapis.com/mcp",
                "headers": { "X-Goog-Api-Key": "gemini-key" }
            },
            "gemini-sse": {
                "url": "https://gemini.googleapis.com/sse"
            }
        }
    }"#;
    std::fs::write(gemini_dir.join("settings.json"), settings_json).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::GeminiCli]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.mcp.len(), 3);

    let stdio = disc.mcp.iter().find(|s| s.name == "gemini-stdio").unwrap();
    assert_eq!(stdio.source, ImportSource::GeminiCli);
    assert_eq!(stdio.origin, gemini_dir.join("settings.json"));
    assert_eq!(stdio.server.command, "python3");
    assert_eq!(stdio.server.args, vec!["mcp_gemini.py"]);
    assert_eq!(stdio.server.env.get("MODEL").unwrap(), "flash");

    let http = disc.mcp.iter().find(|s| s.name == "gemini-http").unwrap();
    assert_eq!(
        http.server.url.as_deref(),
        Some("https://gemini.googleapis.com/mcp")
    );
    assert_eq!(
        http.server.headers.get("X-Goog-Api-Key").unwrap(),
        "gemini-key"
    );

    let sse = disc.mcp.iter().find(|s| s.name == "gemini-sse").unwrap();
    assert_eq!(
        sse.server.url.as_deref(),
        Some("https://gemini.googleapis.com/sse")
    );
    assert!(sse.server.headers.is_empty());
}

#[test]
fn test_vscode_mcp() {
    let temp = TempDir::new("vscode");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let vscode_dir = cwd.join(".vscode");
    std::fs::create_dir_all(&vscode_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mcp_json = r#"{
        "servers": {
            "vscode-stdio": {
                "type": "stdio",
                "command": "node",
                "args": ["vscode-mcp.js"],
                "env": { "PORT": "9000" }
            },
            "vscode-http": {
                "type": "http",
                "url": "https://vscode.api.dev/mcp",
                "headers": { "Authorization": "token" }
            }
        }
    }"#;
    std::fs::write(vscode_dir.join("mcp.json"), mcp_json).unwrap();

    let disc = discover(&home, &cwd, &[ImportSource::VsCode]);
    assert!(
        disc.errors.is_empty(),
        "unexpected errors: {:?}",
        disc.errors
    );
    assert_eq!(disc.mcp.len(), 2);

    let stdio = disc.mcp.iter().find(|s| s.name == "vscode-stdio").unwrap();
    assert_eq!(stdio.source, ImportSource::VsCode);
    assert_eq!(stdio.origin, vscode_dir.join("mcp.json"));
    assert_eq!(stdio.server.command, "node");
    assert_eq!(stdio.server.args, vec!["vscode-mcp.js"]);
    assert_eq!(stdio.server.env.get("PORT").unwrap(), "9000");

    let remote = disc.mcp.iter().find(|s| s.name == "vscode-http").unwrap();
    assert_eq!(remote.source, ImportSource::VsCode);
    assert_eq!(remote.origin, vscode_dir.join("mcp.json"));
    assert_eq!(
        remote.server.url.as_deref(),
        Some("https://vscode.api.dev/mcp")
    );
    assert_eq!(remote.server.headers.get("Authorization").unwrap(), "token");
}

#[test]
fn test_malformed_configs_yield_warnings_never_panic() {
    let temp = TempDir::new("malformed");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");

    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::create_dir_all(cwd.join(".vscode")).unwrap();

    // Malformed JSON / TOML files
    std::fs::write(home.join(".claude.json"), "{ \"mcpServers\": broken }").unwrap();
    std::fs::write(cwd.join(".mcp.json"), "{ \"mcpServers\": 123 }").unwrap();
    std::fs::write(
        home.join(".codex").join("config.toml"),
        "mcp_servers = [invalid",
    )
    .unwrap();
    std::fs::write(
        home.join(".config").join("opencode").join("opencode.json"),
        "not json at all",
    )
    .unwrap();
    std::fs::write(
        home.join(".cursor").join("mcp.json"),
        "{ \"mcpServers\": { \"bad\": 42 } }",
    )
    .unwrap();
    std::fs::write(home.join(".gemini").join("settings.json"), "null").unwrap();
    std::fs::write(
        cwd.join(".vscode").join("mcp.json"),
        "{ \"servers\": { \"incomplete\": {} } }",
    )
    .unwrap();

    let disc = discover(&home, &cwd, ImportSource::all());

    assert!(disc.mcp.is_empty(), "expected 0 valid servers");
    assert!(
        !disc.errors.is_empty(),
        "expected warnings for malformed configs"
    );

    // Ensure we have errors for all affected sources
    let sources_with_errors: std::collections::HashSet<_> =
        disc.errors.iter().map(|e| e.source).collect();
    assert!(sources_with_errors.contains(&ImportSource::ClaudeCode));
    assert!(sources_with_errors.contains(&ImportSource::Codex));
    assert!(sources_with_errors.contains(&ImportSource::OpenCode));
    assert!(sources_with_errors.contains(&ImportSource::Cursor));
    assert!(sources_with_errors.contains(&ImportSource::GeminiCli));
    assert!(sources_with_errors.contains(&ImportSource::VsCode));
}

#[test]
fn test_missing_files_produce_no_errors() {
    let temp = TempDir::new("missing");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let disc = discover(&home, &cwd, ImportSource::all());
    assert!(disc.mcp.is_empty());
    assert!(disc.skills.is_empty());
    assert!(disc.errors.is_empty());
}
