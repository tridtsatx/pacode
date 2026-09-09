use serde_json::json;

use super::*;

#[test]
fn test_manifest_entry_defaults() {
    let toml_lua = r#"
        name = "lua-plug"
        version = "1.0.0"
        kind = "lua"
    "#;
    let manifest: PluginManifest = toml::from_str(toml_lua).unwrap();
    assert_eq!(manifest.entry_path(), PathBuf::from("main.lua"));

    let toml_wasm = r#"
        name = "wasm-plug"
        version = "2.0.0"
        kind = "wasm"
    "#;
    let manifest2: PluginManifest = toml::from_str(toml_wasm).unwrap();
    assert_eq!(manifest2.entry_path(), PathBuf::from("main.wasm"));

    let toml_custom = r#"
        name = "custom"
        version = "0.1.0"
        kind = "lua"
        entry = "src/index.lua"
    "#;
    let manifest3: PluginManifest = toml::from_str(toml_custom).unwrap();
    assert_eq!(manifest3.entry_path(), PathBuf::from("src/index.lua"));
}

#[test]
fn test_command_outcome_deserialize_variants() {
    // String variant
    let s1: CommandOutcome = serde_json::from_str(r#""hello""#).unwrap();
    assert_eq!(s1, CommandOutcome::InsertText("hello".to_string()));

    let s2: CommandOutcome = serde_json::from_str(r#""nothing""#).unwrap();
    assert_eq!(s2, CommandOutcome::Nothing);

    // Object variants
    let o1: CommandOutcome = serde_json::from_str(r#"{"insert_text": "text"}"#).unwrap();
    assert_eq!(o1, CommandOutcome::InsertText("text".to_string()));

    let o2: CommandOutcome = serde_json::from_str(r#"{"send_prompt": "prompt"}"#).unwrap();
    assert_eq!(o2, CommandOutcome::SendPrompt("prompt".to_string()));

    let o3: CommandOutcome = serde_json::from_str(r#"{"nothing": true}"#).unwrap();
    assert_eq!(o3, CommandOutcome::Nothing);

    let o4: CommandOutcome =
        serde_json::from_str(r#"{"type": "insert_text", "text": "foo"}"#).unwrap();
    assert_eq!(o4, CommandOutcome::InsertText("foo".to_string()));
}

#[test]
fn test_hook_result_deserialize_variants() {
    let r1: HookResult = serde_json::from_str(r#""continue""#).unwrap();
    assert_eq!(r1, HookResult::Continue);

    let r2: HookResult = serde_json::from_str(r#"{"continue": true}"#).unwrap();
    assert_eq!(r2, HookResult::Continue);

    let r3: HookResult = serde_json::from_str(r#"{"deny": "forbidden"}"#).unwrap();
    assert_eq!(
        r3,
        HookResult::Deny {
            reason: "forbidden".to_string()
        }
    );

    let r4: HookResult = serde_json::from_str(r#"{"deny": {"reason": "forbidden2"}}"#).unwrap();
    assert_eq!(
        r4,
        HookResult::Deny {
            reason: "forbidden2".to_string()
        }
    );

    let r5: HookResult = serde_json::from_str(r#"{"modify_input": {"key": "val"}}"#).unwrap();
    assert_eq!(r5, HookResult::ModifyInput(json!({ "key": "val" })));
}

#[test]
fn test_hook_event_serde_roundtrip() {
    let e = HookEvent::PreToolCall {
        name: "test".to_string(),
        input: json!({ "a": 1 }),
    };
    let json_str = serde_json::to_string(&e).unwrap();
    let e2: HookEvent = serde_json::from_str(&json_str).unwrap();
    assert_eq!(e, e2);

    let e_turn = HookEvent::TurnEnd {
        duration_ms: 1234,
        output_tokens: 56,
    };
    let json_str2 = serde_json::to_string(&e_turn).unwrap();
    let e_turn2: HookEvent = serde_json::from_str(&json_str2).unwrap();
    assert_eq!(e_turn, e_turn2);
}
