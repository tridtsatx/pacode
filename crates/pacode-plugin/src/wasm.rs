//! WebAssembly plugin implementation using `wasmtime` and WASI p2.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::error::PluginError;
use crate::runtime::PluginRuntime;
use crate::sink::UiSink;
use crate::types::{
    CommandOutcome, HookEvent, HookResult, PluginCommandDef, PluginKind, PluginManifest,
    PluginToolDef,
};

wasmtime::component::bindgen!({
    world: "plugin",
    path: "wit/pacode.wit",
    exports: {
        default: async,
    },
});

static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

pub fn get_wasm_engine() -> Result<&'static Engine, PluginError> {
    let res = ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.consume_fuel(true);
        Engine::new(&config).map_err(|e| e.to_string())
    });
    match res {
        Ok(engine) => Ok(engine),
        Err(err) => Err(PluginError::Wasm(format!(
            "failed to initialize wasm engine: {err}"
        ))),
    }
}

pub struct WasmState {
    ctx: WasiCtx,
    table: ResourceTable,
    pub limits: StoreLimits,
    ui_sink: Arc<dyn UiSink>,
}

impl WasiView for WasmState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl PluginImports for WasmState {
    fn toast(&mut self, text: String) {
        self.ui_sink.toast(&text);
    }

    fn status(&mut self, text: String) {
        self.ui_sink.status(&text);
    }
}

struct WasmPluginInner {
    store: Store<WasmState>,
    plugin: Plugin,
}

pub struct WasmPlugin {
    manifest: PluginManifest,
    tools: Vec<PluginToolDef>,
    commands: Vec<PluginCommandDef>,
    inner: Arc<Mutex<WasmPluginInner>>,
    timeout: Duration,
}

impl WasmPlugin {
    pub async fn load(
        manifest: &PluginManifest,
        entry_path: &Path,
        wasm_memory_mb: u32,
        hook_timeout_ms: u64,
        ui_sink: Arc<dyn UiSink>,
    ) -> Result<Self, PluginError> {
        let bytes = std::fs::read(entry_path).map_err(PluginError::Io)?;
        Self::load_from_bytes(manifest, &bytes, wasm_memory_mb, hook_timeout_ms, ui_sink).await
    }

    pub async fn load_from_bytes(
        manifest: &PluginManifest,
        bytes: &[u8],
        wasm_memory_mb: u32,
        hook_timeout_ms: u64,
        ui_sink: Arc<dyn UiSink>,
    ) -> Result<Self, PluginError> {
        let engine = get_wasm_engine()?;
        let component =
            Component::new(engine, bytes).map_err(|e| PluginError::Wasm(e.to_string()))?;

        let wasi_ctx = WasiCtxBuilder::new().build();
        let table = ResourceTable::new();
        let mem_limit_bytes = (wasm_memory_mb as usize).saturating_mul(1024 * 1024);
        let limits = StoreLimitsBuilder::new()
            .memory_size(mem_limit_bytes)
            .build();

        let state = WasmState {
            ctx: wasi_ctx,
            table,
            limits,
            ui_sink,
        };

        let mut store = Store::new(engine, state);
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(1_000_000_000)
            .map_err(|e| PluginError::Wasm(e.to_string()))?;

        let mut linker = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(|e| PluginError::Wasm(e.to_string()))?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| PluginError::Wasm(e.to_string()))?;

        let plugin = Plugin::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| PluginError::Wasm(e.to_string()))?;

        let raw_tools = plugin
            .call_get_tools(&mut store)
            .await
            .map_err(map_wasm_error)?;

        let tools = raw_tools
            .into_iter()
            .map(|t| PluginToolDef {
                name: t.name,
                description: t.description,
                schema: serde_json::from_str(&t.schema).unwrap_or(Value::Null),
            })
            .collect();

        let raw_commands = plugin
            .call_get_commands(&mut store)
            .await
            .map_err(map_wasm_error)?;

        let commands = raw_commands
            .into_iter()
            .map(|c| PluginCommandDef {
                name: c.name,
                description: c.description,
            })
            .collect();

        Ok(Self {
            manifest: manifest.clone(),
            tools,
            commands,
            inner: Arc::new(Mutex::new(WasmPluginInner { store, plugin })),
            timeout: Duration::from_millis(hook_timeout_ms),
        })
    }
}

#[async_trait]
impl PluginRuntime for WasmPlugin {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn version(&self) -> &str {
        &self.manifest.version
    }

    fn kind(&self) -> PluginKind {
        PluginKind::Wasm
    }

    fn description(&self) -> Option<&str> {
        self.manifest.description.as_deref()
    }

    fn tools(&self) -> Vec<PluginToolDef> {
        self.tools.clone()
    }

    fn commands(&self) -> Vec<PluginCommandDef> {
        self.commands.clone()
    }

    async fn call_tool(&self, name: &str, input: Value) -> Result<Value, PluginError> {
        let input_json =
            serde_json::to_string(&input).map_err(|e| PluginError::Serialization(e.to_string()))?;
        let mut guard = self.inner.lock().await;
        let WasmPluginInner { store, plugin } = &mut *guard;

        let _ = store.set_fuel(1_000_000_000);
        let fut = plugin.call_call_tool(store, name, &input_json);

        let call_res = tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| PluginError::Timeout)?
            .map_err(map_wasm_error)?;

        match call_res {
            Ok(out_json) => serde_json::from_str(&out_json)
                .map_err(|e| PluginError::Serialization(e.to_string())),
            Err(err) => Err(PluginError::Wasm(err)),
        }
    }

    async fn run_command(&self, name: &str, args: String) -> Result<CommandOutcome, PluginError> {
        let mut guard = self.inner.lock().await;
        let WasmPluginInner { store, plugin } = &mut *guard;

        let _ = store.set_fuel(1_000_000_000);
        let fut = plugin.call_run_command(store, name, &args);

        let call_res = tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| PluginError::Timeout)?
            .map_err(map_wasm_error)?;

        match call_res {
            Ok(out_json) => serde_json::from_str(&out_json)
                .map_err(|e| PluginError::Serialization(e.to_string())),
            Err(err) => Err(PluginError::Wasm(err)),
        }
    }

    async fn hook(&self, event: HookEvent) -> Result<HookResult, PluginError> {
        let event_json =
            serde_json::to_string(&event).map_err(|e| PluginError::Serialization(e.to_string()))?;
        let mut guard = self.inner.lock().await;
        let WasmPluginInner { store, plugin } = &mut *guard;

        let _ = store.set_fuel(1_000_000_000);
        let fut = plugin.call_hook(store, &event_json);

        let call_res = tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| PluginError::Timeout)?
            .map_err(map_wasm_error)?;

        match call_res {
            Ok(out_json) => serde_json::from_str(&out_json)
                .map_err(|e| PluginError::Serialization(e.to_string())),
            Err(err) => Err(PluginError::Wasm(err)),
        }
    }
}

fn map_wasm_error(err: wasmtime::Error) -> PluginError {
    if let Some(trap) = err.downcast_ref::<wasmtime::Trap>() {
        match trap {
            wasmtime::Trap::OutOfFuel => return PluginError::Timeout,
            wasmtime::Trap::MemoryOutOfBounds => {
                return PluginError::MemoryLimit("memory out of bounds".to_string());
            }
            _ => {}
        }
    }
    let msg = err.to_string();
    if msg.contains("all fuel consumed") || msg.contains("out of fuel") {
        PluginError::Timeout
    } else if msg.contains("memory") && (msg.contains("limit") || msg.contains("grow")) {
        PluginError::MemoryLimit(msg)
    } else {
        PluginError::Wasm(msg)
    }
}
