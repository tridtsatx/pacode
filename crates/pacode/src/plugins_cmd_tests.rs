use std::fs;
use std::path::PathBuf;

use pacode_types::Config;
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn test_plugins_list_with_echo_lua_fixture() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("echo-lua");
    fs::create_dir_all(&plugin_dir).unwrap();

    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("pacode-plugin")
        .join("tests")
        .join("fixtures")
        .join("echo-lua");

    let manifest = fs::read_to_string(fixture_dir.join("plugin.toml")).unwrap();
    let script = fs::read_to_string(fixture_dir.join("main.lua")).unwrap();

    fs::write(plugin_dir.join("plugin.toml"), manifest).unwrap();
    fs::write(plugin_dir.join("main.lua"), script).unwrap();

    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.dirs = vec![dir.path().to_path_buf()];

    let mut out = Vec::new();
    let has_errors = list_plugins(&config, &mut out).await.unwrap();

    assert!(!has_errors);
    let out_str = String::from_utf8(out).unwrap();

    assert!(out_str.contains("NAME"));
    assert!(out_str.contains("VERSION"));
    assert!(out_str.contains("KIND"));
    assert!(out_str.contains("TOOLS"));
    assert!(out_str.contains("COMMANDS"));
    assert!(out_str.contains("ERROR"));

    assert!(out_str.contains("echo-lua"));
    assert!(out_str.contains("0.1.0"));
    assert!(out_str.contains("lua"));
    assert!(out_str.contains("echo_tool"));
    assert!(out_str.contains("echo_cmd"));
}

#[tokio::test]
async fn test_plugins_list_with_broken_plugin() {
    let dir = tempdir().unwrap();
    let broken_dir = dir.path().join("broken-plugin");
    fs::create_dir_all(&broken_dir).unwrap();

    fs::write(broken_dir.join("plugin.toml"), "invalid toml :::").unwrap();

    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.dirs = vec![dir.path().to_path_buf()];

    let mut out = Vec::new();
    let has_errors = list_plugins(&config, &mut out).await.unwrap();

    assert!(has_errors);
    let out_str = String::from_utf8(out).unwrap();
    assert!(out_str.contains("broken-plugin"));
}
