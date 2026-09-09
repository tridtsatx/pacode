use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use pacode_mcp::{McpClient, McpError, McpPool, fingerprint, split_tool_name, tool_name};
use pacode_types::McpServerConfig;
use serde_json::json;

fn has_python3() -> bool {
    match std::process::Command::new("python3")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => true,
        _ => {
            eprintln!("Skipping test: python3 --version failed or python3 not installed");
            false
        }
    }
}

fn fake_mcp_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_mcp.py")
}

#[test]
fn test_tool_name_and_split_unit() {
    assert_eq!(tool_name("server1", "echo"), "server1__echo");
    assert_eq!(split_tool_name("server1__echo"), Some(("server1", "echo")));
    assert_eq!(split_tool_name("no_separator"), None);
    assert_eq!(
        split_tool_name("server1__tool__extra"),
        Some(("server1", "tool__extra"))
    );
}

#[tokio::test]
async fn test_client_start_list_call() {
    if !has_python3() {
        return;
    }

    let script = fake_mcp_path();
    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let client = McpClient::start("fake", &cfg, None)
        .await
        .expect("start client");
    assert_eq!(client.name(), "fake");
    assert!(client.is_alive());

    // list tools (both pages: page 1 = echo, page 2 = fail, slow)
    let tools = client.list_tools().await.expect("list tools");
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[1].name, "fail");
    assert_eq!(tools[2].name, "slow");

    // call echo
    let echo_res = client
        .call_tool("echo", json!({"msg": "hello"}), Duration::from_secs(5))
        .await
        .expect("call echo");
    assert!(!echo_res.is_error);
    assert!(echo_res.content.contains("\"msg\": \"hello\""));

    // call fail -> is_error
    let fail_res = client
        .call_tool("fail", json!({}), Duration::from_secs(5))
        .await
        .expect("call fail");
    assert!(fail_res.is_error);
    assert!(fail_res.content.contains("failed intentionally"));

    client.shutdown().await;
    assert!(!client.is_alive());
}

#[tokio::test]
async fn test_client_timeout_on_slow_tool() {
    if !has_python3() {
        return;
    }

    let script = fake_mcp_path();
    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: false,
        timeout_secs: 1,
        ..Default::default()
    };

    let client = McpClient::start("slow_server", &cfg, None)
        .await
        .expect("start slow client");
    assert!(client.is_alive());

    let res = client
        .call_tool("slow", json!({}), Duration::from_secs(1))
        .await;
    match res {
        Err(McpError::Timeout(secs)) => {
            assert_eq!(secs, 1);
        }
        other => panic!("expected McpError::Timeout(1), got: {other:?}"),
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_pool_lazy_start_and_disk_cache_reuse() {
    if !has_python3() {
        return;
    }

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let cache_dir = temp_dir.path().to_path_buf();
    let script = fake_mcp_path();

    let fake_cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: true,
        timeout_secs: 5,
        ..Default::default()
    };

    // First pool instance: cold cache -> starts server and populates disk cache
    let mut servers1 = BTreeMap::new();
    servers1.insert("fake".to_string(), fake_cfg.clone());
    let pool1 = McpPool::new(servers1, Some(cache_dir.clone()), None);

    assert_eq!(pool1.server_names(), vec!["fake".to_string()]);
    let tools1 = pool1.list_all_tools().await;
    assert_eq!(tools1.len(), 3);
    assert_eq!(tools1[0].0, "fake");
    assert_eq!(tools1[0].1.name, "echo");

    // Verify cache file exists on disk
    let cache_file = cache_dir.join("fake.json");
    assert!(cache_file.exists());

    // Test tool call via pool
    let call_res = pool1
        .call("fake", "echo", json!({"data": "test"}))
        .await
        .expect("pool call echo");
    assert!(!call_res.is_error);
    assert!(call_res.content.contains("test"));

    pool1.shutdown().await;

    // Second pool instance with the same cache_dir:
    // Verify schema cache reuse by configuring a bogus command that would fail if started.
    let bogus_cfg = McpServerConfig {
        command: "bogus_command_that_does_not_exist_xyz_12345".to_string(),
        args: vec![],
        env: BTreeMap::new(),
        lazy: true,
        timeout_secs: 5,
        ..Default::default()
    };

    // Update the disk cache entry for "fake" to match the bogus config fingerprint
    let mut disk_val: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_file).unwrap()).unwrap();
    disk_val["fingerprint"] = json!(fingerprint(&bogus_cfg));
    std::fs::write(&cache_file, serde_json::to_string(&disk_val).unwrap()).unwrap();

    let mut servers2 = BTreeMap::new();
    servers2.insert("fake".to_string(), bogus_cfg);
    let pool2 = McpPool::new(servers2, Some(cache_dir.clone()), None);

    // list_all_tools uses the disk cache without starting the bogus command!
    let tools2 = pool2.list_all_tools().await;
    assert_eq!(tools2.len(), 3);
    assert_eq!(tools2[0].0, "fake");
    assert_eq!(tools2[0].1.name, "echo");

    // But calling a tool on pool2 requires starting the process, which fails with Spawn!
    let call_err = pool2
        .call("fake", "echo", json!({}))
        .await
        .expect_err("bogus command must fail to spawn");
    assert!(matches!(call_err, McpError::Spawn { .. }));

    pool2.shutdown().await;
}

#[tokio::test]
async fn test_pool_restart_on_dead_client() {
    if !has_python3() {
        return;
    }

    let temp_dir = tempfile::tempdir().expect("create tempdir");
    let flag_file = temp_dir.path().join("crash_flag.txt");
    let script = fake_mcp_path();

    let fake_cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: true,
        timeout_secs: 5,
        ..Default::default()
    };

    let mut servers = BTreeMap::new();
    servers.insert("fake".to_string(), fake_cfg);
    let pool = McpPool::new(servers, None, None);

    // crash_once crashes the first time, writing flag_file.
    // pool.call detects dead client and retries once, which succeeds.
    let res = pool
        .call(
            "fake",
            "crash_once",
            json!({"flag_file": flag_file.to_str().unwrap()}),
        )
        .await
        .expect("pool must restart dead client and retry call");
    assert!(!res.is_error);
    assert_eq!(res.content, "recovered after restart");

    pool.shutdown().await;
}

#[tokio::test]
async fn test_server_request_replies_method_not_found() {
    if !has_python3() {
        return;
    }

    let script = fake_mcp_path();
    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let client = McpClient::start("server_req_test", &cfg, None)
        .await
        .expect("start client");

    let res = client
        .call_tool("test_server_request", json!({}), Duration::from_secs(5))
        .await
        .expect("call test_server_request");
    assert!(!res.is_error);
    assert_eq!(res.content, "server request handled successfully");

    client.shutdown().await;
}

#[tokio::test]
async fn test_client_resources_and_prompts_stdio() {
    if !has_python3() {
        return;
    }

    let script = fake_mcp_path();
    let mut env = BTreeMap::new();
    env.insert("FAKE_MCP_RESOURCES".to_string(), "1".to_string());
    env.insert("FAKE_MCP_PROMPTS".to_string(), "1".to_string());

    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env,
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let client = McpClient::start("res_prompt_test", &cfg, None)
        .await
        .expect("start client");

    // Resources
    let resources = client.list_resources().await.expect("list resources");
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].uri, "fake://resource1");
    assert_eq!(resources[0].name, "Resource 1");
    assert_eq!(resources[1].uri, "fake://resource2");

    let text1 = client
        .read_resource("fake://resource1")
        .await
        .expect("read resource 1");
    assert_eq!(text1, "content of resource1");

    let text2 = client
        .read_resource("fake://resource2")
        .await
        .expect("read resource 2");
    assert_eq!(text2, "[blob: image/png]");

    // Prompts
    let prompts = client.list_prompts().await.expect("list prompts");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "test_prompt");
    assert_eq!(prompts[0].arguments.len(), 1);
    assert_eq!(prompts[0].arguments[0].name, "topic");

    let rendered = client
        .get_prompt("test_prompt", json!({"topic": "Rust concurrency"}))
        .await
        .expect("get prompt");
    assert_eq!(rendered, "Tell me about Rust concurrency");

    client.shutdown().await;
}

struct FakeSamplingHandler;

#[async_trait::async_trait]
impl pacode_mcp::SamplingHandler for FakeSamplingHandler {
    async fn create_message(
        &self,
        req: pacode_mcp::SamplingRequest,
    ) -> Result<pacode_mcp::SamplingResponse, McpError> {
        // Verify cap was applied from 4096 -> 2048
        assert_eq!(req.max_tokens, Some(2048));
        Ok(pacode_mcp::SamplingResponse::text(
            "model-test",
            "fake AI answer",
        ))
    }
}

#[tokio::test]
async fn test_sampling_roundtrip_stdio() {
    if !has_python3() {
        return;
    }

    let script = fake_mcp_path();
    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: false,
        timeout_secs: 5,
        ..Default::default()
    };

    let mut servers = BTreeMap::new();
    servers.insert("fake".to_string(), cfg);
    let pool = McpPool::new(servers, None, None);
    pool.set_sampling_handler(std::sync::Arc::new(FakeSamplingHandler));

    let res = pool
        .call("fake", "test_sampling", json!({}))
        .await
        .expect("call test_sampling");

    assert!(!res.is_error);
    assert!(
        res.content
            .contains("sampling response received: fake AI answer")
    );

    pool.shutdown().await;
}

#[tokio::test]
async fn test_pool_statuses_and_enabled_restart() {
    if !has_python3() {
        return;
    }

    let script = fake_mcp_path();
    let cfg = McpServerConfig {
        command: "python3".to_string(),
        args: vec![script.to_str().unwrap().to_string()],
        env: BTreeMap::new(),
        lazy: true,
        timeout_secs: 5,
        ..Default::default()
    };

    let mut servers = BTreeMap::new();
    servers.insert("fake".to_string(), cfg);
    let pool = McpPool::new(servers, None, None);

    // Initial status: Stopped
    let statuses = pool.statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].0, "fake");
    assert_eq!(statuses[0].1, pacode_mcp::ServerStatus::Stopped);

    // Trigger lazy start via list_all_tools
    let tools = pool.list_all_tools().await;
    assert_eq!(tools.len(), 3);

    let statuses = pool.statuses();
    match &statuses[0].1 {
        pacode_mcp::ServerStatus::Ready {
            tools,
            resources,
            prompts,
        } => {
            assert_eq!(*tools, 3);
            assert_eq!(*resources, 0);
            assert_eq!(*prompts, 0);
        }
        other => panic!("expected Ready, got: {other:?}"),
    }

    // Disable server
    pool.set_enabled("fake", false).await.expect("disable");
    let statuses = pool.statuses();
    assert_eq!(statuses[0].1, pacode_mcp::ServerStatus::Stopped);

    // Disabled servers advertise nothing
    let tools = pool.list_all_tools().await;
    assert!(tools.is_empty());

    // Re-enable and restart
    pool.set_enabled("fake", true).await.expect("enable");
    pool.restart("fake").await.expect("restart");

    let statuses = pool.statuses();
    match &statuses[0].1 {
        pacode_mcp::ServerStatus::Ready { tools, .. } => {
            assert_eq!(*tools, 3);
        }
        other => panic!("expected Ready after restart, got: {other:?}"),
    }

    pool.shutdown().await;
}
