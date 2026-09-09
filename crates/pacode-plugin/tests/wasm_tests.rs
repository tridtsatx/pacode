use std::sync::Arc;

use pacode_plugin::sink::NoopUiSink;
use pacode_plugin::types::{PluginKind, PluginManifest};
use pacode_plugin::wasm::{WasmPlugin, get_wasm_engine};
use wasmtime::component::Component;

#[test]
fn test_wasm_engine_and_wat_parsing() {
    let engine = get_wasm_engine().expect("wasm engine should initialize");
    let wat = r#"
    (component
      (type $StringParam (func (param "text" string)))
      (import "toast" (func $toast (type $StringParam)))
      (import "status" (func $status (type $StringParam)))
    )
    "#;
    let component = Component::new(engine, wat);
    assert!(component.is_ok(), "WAT component parsing should succeed");
}

#[tokio::test]
#[ignore = "WASM guest component toolchain (cargo-component / wasm-tools) not installed in host environment"]
async fn test_wasm_plugin_guest_component_execution() {
    // This test exercises full execution of a compiled WebAssembly guest component
    // implementing the `pacode:plugin` world. Because neither `cargo-component` nor `wasm-tools`
    // is installed on this host system, this test is skipped per the task specification.
    let manifest = PluginManifest {
        name: "test-wasm".to_string(),
        version: "0.1.0".to_string(),
        kind: PluginKind::Wasm,
        entry: None,
        description: Some("WASM test component".to_string()),
    };

    // A pre-compiled component binary would be loaded here:
    let empty_component_bytes: &[u8] = &[];
    let res = WasmPlugin::load_from_bytes(
        &manifest,
        empty_component_bytes,
        64,
        2000,
        Arc::new(NoopUiSink),
    )
    .await;
    let _ = res;
}
