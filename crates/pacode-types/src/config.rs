//! Configuration structures (`~/.config/pacode/config.toml`). Loading lives in
//! `pacode-config`; this module only defines the shape and defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
pub use serde_json;
use serde_json::Value;

use crate::model::{Effort, ModelRoute, Pricing};
use crate::state::Mode;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: ProviderDefaults,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Keyed by model id (without provider prefix).
    pub pricing: BTreeMap<String, Pricing>,
    pub ui: UiConfig,
    pub exec: ExecConfig,
    pub agents: AgentsConfig,
    pub limits: LimitsConfig,
    pub daemon: DaemonConfig,
    pub context: ContextConfig,
    pub permissions: PermissionsConfig,
    pub mcp: McpConfig,
    pub session: SessionConfig,
    pub plugins: PluginsConfig,
    pub skills: SkillsConfig,
    pub theme: ThemeConfig,
    pub font: FontConfig,
    pub keys: KeysConfig,
    pub web: WebConfig,
    pub hooks: HooksConfig,
    /// Zero-data-retention routing (`zdr = "always" | "session" | "off"`).
    pub zdr: ZdrMode,
    /// `[tools.<name>]`: per-tool toggles. `enabled = false` removes the tool
    /// from the schema sent to the model — built-in names as well as plugin,
    /// skill and MCP `<server>__<tool>` names. Applied when the daemon wires
    /// its registries, so changes need a restart.
    pub tools: BTreeMap<String, ToolToggle>,
}

impl Config {
    /// Default route from `[provider].default`, parsed against configured providers.
    pub fn default_route(&self) -> Option<ModelRoute> {
        let raw = self.provider.default.as_deref()?;
        let known = self.providers.keys().map(String::as_str);
        let fallback = self.default_provider();
        ModelRoute::parse(raw, known, fallback)
    }

    /// Provider ids in the order a bare model name should try them: highest
    /// `priority` first, then the alphabetically first id.
    ///
    /// A single source of truth for provider ordering, so the CLI, the daemon
    /// and the registry cannot disagree about which provider a bare model id
    /// lands on. `BTreeMap` iteration is already alphabetical, so only the
    /// priority needs reordering — and only when it is actually set.
    pub fn provider_order(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.providers.keys().map(String::as_str).collect();
        if ids.iter().any(|id| self.providers[*id].priority != 0) {
            ids.sort_by(|a, b| {
                self.providers[*b]
                    .priority
                    .cmp(&self.providers[*a].priority)
                    .then_with(|| a.cmp(b))
            });
        }
        ids
    }

    /// The provider a bare model id belongs to: the highest-priority entry that
    /// names one in `default_model`.
    pub fn default_provider(&self) -> Option<&str> {
        self.provider_order()
            .into_iter()
            .find(|id| has_default_model(&self.providers[*id]))
    }

    pub fn pricing_for(&self, model: &str) -> Option<Pricing> {
        self.pricing.get(model).copied()
    }

    /// Whether `name` is exposed to the model; tools without an entry are enabled.
    pub fn tool_enabled(&self, name: &str) -> bool {
        self.tools.get(name).map(|t| t.enabled).unwrap_or(true)
    }

    /// Names disabled via `[tools.<name>] enabled = false`.
    pub fn disabled_tool_names(&self) -> impl Iterator<Item = &str> {
        self.tools
            .iter()
            .filter(|(_, toggle)| !toggle.enabled)
            .map(|(name, _)| name.as_str())
    }
}

/// `[tools.<name>]` entry; a present-but-empty table means enabled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolToggle {
    pub enabled: bool,
}

impl Default for ToolToggle {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderDefaults {
    /// `provider/model`.
    pub default: Option<String>,
    pub effort: Effort,
    /// Base idle timeout of a streaming response, scaled by effort.
    pub stream_idle_secs: u64,
    pub max_retries: u32,
    /// Time-to-live in seconds for cached model catalog (background refresh).
    pub catalog_ttl_secs: u64,
    pub proxy: Option<String>,
}

impl Default for ProviderDefaults {
    fn default() -> Self {
        Self {
            default: None,
            effort: Effort::Medium,
            stream_idle_secs: 180,
            max_retries: 5,
            catalog_ttl_secs: 86400,
            proxy: None,
        }
    }
}

/// Zero-data-retention mode (spec §3). `always` hard-locks the session to
/// ZDR-capable models; `session` lets the user toggle it (Alt+Z); `off`
/// disables the feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZdrMode {
    Always,
    #[default]
    Session,
    Off,
}

impl ZdrMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Session => "session",
            Self::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "always" => Some(Self::Always),
            "session" => Some(Self::Session),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Deployment kind of a provider entry. This is the *name* of a wire dialect,
/// not a deployment: a self-hosted gateway is `openai` whatever host it runs
/// on, and the same OpenAI dialect serves a dozen unrelated vendors. New
/// dialects get their own [`ProviderDialect`]; this enum stays closed at the
/// five names pacode shipped with, so an old `kind = "openai"` keeps loading
/// forever. Anything generic is spelled `openai-compatible`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    #[serde(alias = "openai")]
    OpenAi,
    Anthropic,
    Codex,
    Devin,
    #[serde(alias = "opencode")]
    OpenCode,
}

/// Wire protocol an adapter speaks: how a request body is shaped and how a
/// response stream is decoded. Deliberately *not* a deployment descriptor —
/// `base_url`, credentials, headers and timeouts are deployment and live in
/// [`ProviderConfig`], so a new host on an existing dialect needs no code.
///
/// `openai_compatible` is the default and the universal escape hatch: any
/// endpoint that speaks `/chat/completions` is reachable by declaration alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderDialect {
    // Spelled out rather than derived: `rename_all = "snake_case"` would make
    // this `open_ai_compatible`, which matches neither `as_str` nor `parse` and
    // is not what anyone would write in a config.
    #[default]
    #[serde(
        rename = "openai_compatible",
        alias = "openai",
        alias = "openai-compat"
    )]
    OpenAiCompatible,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "devin")]
    Devin,
    #[serde(rename = "opencode_zen", alias = "opencode", alias = "opencode-zen")]
    OpenCodeZen,
}

impl ProviderDialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
            Self::Devin => "devin",
            Self::OpenCodeZen => "opencode_zen",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "openai_compatible" | "openai" | "openai-compat" => Some(Self::OpenAiCompatible),
            "anthropic" => Some(Self::Anthropic),
            "codex" => Some(Self::Codex),
            "devin" => Some(Self::Devin),
            "opencode_zen" | "opencode" => Some(Self::OpenCodeZen),
            _ => None,
        }
    }

    /// The dialect a legacy `kind = ...` entry means. Frozen: this mapping is
    /// the backward-compatibility contract for configs written before
    /// `dialect` existed.
    pub fn from_kind(kind: ProviderKind) -> Self {
        match kind {
            ProviderKind::OpenAi => Self::OpenAiCompatible,
            ProviderKind::Anthropic => Self::Anthropic,
            ProviderKind::Codex => Self::Codex,
            ProviderKind::Devin => Self::Devin,
            ProviderKind::OpenCode => Self::OpenCodeZen,
        }
    }
}

/// How the reasoning/thinking channel is expressed on the wire. `Auto` is the
/// historical behaviour — send `reasoning_effort` when the model is flagged as
/// a reasoning model — and stays the default so an unset axis changes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    /// Follow the existing `reasoning` flag / model heuristic.
    #[default]
    Auto,
    /// Send the effort field even for a model not flagged as reasoning.
    Always,
    /// Never send it, even for a reasoning model.
    Never,
}

impl ThinkingMode {
    /// Whether the effort field is sent for a model whose reasoning flag
    /// resolved to `reasoning`.
    pub fn sends_for_reasoning_model(self, reasoning: bool) -> bool {
        match self {
            Self::Auto => reasoning,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// Whether file mutations are offered to the model as one `apply_patch` tool
/// (what the GPT-5 family expects) or as pacode's separate `edit`/`write`/
/// `multi_edit` trio. `Keep` is the default and the current behaviour.
///
/// There is deliberately no `ApplyPatch` variant yet. Advertising a tool name
/// pacode cannot dispatch would make the model emit calls that come back as
/// `unknown tool`, so the axis stops at what is actually implementable: keep
/// the real tools, or offer no way to write files at all. Collapsing the trio
/// becomes a variant once the dispatcher can parse a unified diff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchToolType {
    /// Advertise the tools pacode actually has.
    #[default]
    Keep,
    /// Hide the mutation tools entirely (the model gets no way to write files).
    None,
}

/// Tool names that mutate the working tree, i.e. the ones [`ApplyPatchToolType`]
/// governs.
pub const MUTATION_TOOLS: &[&str] = &["edit", "write", "multi_edit"];

/// Whether the provider's server-side search tool is exposed to the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchToolType {
    /// Advertise pacode's own `websearch` tool and nothing else (default).
    #[default]
    Native,
    /// Drop pacode's `websearch` and let the provider's built-in search run.
    Provider,
    /// Neither.
    Disabled,
}

/// What the provider accepts in a tool-result message. `Text` is the default and
/// matches what pacode sends today (the rendered text, not the raw block).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultMode {
    #[default]
    Text,
    /// Also inline the structured payload as JSON next to the text.
    TextWithJson,
}

/// How image blocks are put on the wire. `DataUrl` is the current behaviour
/// (inline base64 `image_url` parts); `TextOnly` degrades every image to its
/// placeholder, which is the safe setting for a provider that silently
/// mis-renders an unexpected part type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransport {
    #[default]
    DataUrl,
    TextOnly,
}

/// Typed compatibility axes — a closed set, not a `BTreeMap<String, bool>`:
/// every axis is a value pacode acts on, each one defaults to what pacode
/// already did when the axis is absent, and `deny_unknown_fields` makes a typo
/// in a compat key a load-time error instead of a silently ignored setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderCompat {
    /// Default [`ThinkingMode::Auto`] — `reasoning_effort` goes out exactly when
    /// the model is flagged as reasoning (config, model entry, or heuristic).
    pub thinking_mode: ThinkingMode,
    /// Default [`ApplyPatchToolType::Keep`] — `edit`/`write`/`multi_edit` are
    /// advertised unchanged.
    pub apply_patch_tool_type: ApplyPatchToolType,
    /// Default [`WebSearchToolType::Native`] — pacode's own `websearch` tool.
    pub web_search_tool_type: WebSearchToolType,
    /// Default [`ToolResultMode::Text`] — the rendered text only.
    pub tool_result_mode: ToolResultMode,
    /// Default [`ImageTransport::DataUrl`] — inline base64 image parts.
    pub image_transport: ImageTransport,
    /// Default `false` — `max_tokens` is sent whenever the turn carries a cap.
    pub omit_max_output_tokens: bool,
}

/// Hand-written because two of the seven axes default to `true`, which
/// `#[derive(Default)]` would flip to `false` — and a provider that reports
/// usage is the common case, not the exception. Every value here is what
/// pacode did before the axis existed, so an entry that sets none is unchanged.
impl Default for ProviderCompat {
    fn default() -> Self {
        Self {
            thinking_mode: ThinkingMode::default(),
            apply_patch_tool_type: ApplyPatchToolType::default(),
            web_search_tool_type: WebSearchToolType::default(),
            tool_result_mode: ToolResultMode::default(),
            image_transport: ImageTransport::default(),
            omit_max_output_tokens: false,
        }
    }
}

impl ProviderCompat {
    /// Whether a `max_output_tokens` cap from the turn reaches the wire.
    pub fn sends_max_output_tokens(&self) -> bool {
        !self.omit_max_output_tokens
    }

    /// Whether the effort/thinking field is sent for a model whose reasoning
    /// flag resolved to `reasoning`.
    pub fn sends_thinking_for(&self, reasoning: bool) -> bool {
        self.thinking_mode.sends_for_reasoning_model(reasoning)
    }

    /// The wire tool list: the entry tools filtered by this provider's axes.
    /// Empty when the model gets no tools at all.
    ///
    /// `Native` keeps pacode's `websearch` (today's behaviour); `Provider`
    /// drops it because the gateway runs its own search and pacode's tool only
    /// costs the model a round trip for the same answer; `Disabled` drops it
    /// too, and is the honest setting for a provider that has no search at all.
    /// [`ApplyPatchToolType::None`] likewise removes the file-mutation tools.
    pub fn wire_tools(&self, tools: &[crate::message::ToolDefinition]) -> Vec<WireTool> {
        if self.web_search_tool_type != WebSearchToolType::Native
            && tools.iter().any(|t| t.name == "websearch")
        {
            return self
                .non_mutation_tools(tools)
                .into_iter()
                .filter(|t| t.name != "websearch")
                .collect();
        }
        self.non_mutation_tools(tools)
    }

    fn non_mutation_tools(
        &self,
        tools: &[crate::message::ToolDefinition],
    ) -> Vec<crate::message::ToolDefinition> {
        if self.apply_patch_tool_type == ApplyPatchToolType::None {
            return tools
                .iter()
                .filter(|t| !MUTATION_TOOLS.contains(&t.name.as_str()))
                .cloned()
                .collect();
        }
        tools.to_vec()
    }
}

/// One entry of a wire tool list: a real tool definition that survived the
/// provider's compatibility filtering.
pub type WireTool = crate::message::ToolDefinition;

/// JSON schema of the synthetic `apply_patch` tool: one unified diff. Chosen
/// to match the shape the GPT-5 family is trained on. Kept next to the axis
/// that will use it, so adding the variant later is a single change.
#[allow(dead_code)]
pub fn apply_patch_input_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "patch": {
                "type": "string",
                "description": "A unified diff (the `---`/`+++` headers are optional) applied to the working tree.",
            }
        },
        "required": ["patch"],
    })
}

/// Per-provider transport tuning. `None` means "inherit the global
/// `[provider]` default", so an axis that is absent changes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderTimeouts {
    /// TCP connect timeout; `None` uses the adapter's own connect bound.
    pub connect_secs: Option<u64>,
    /// Idle timeout of a streaming response, before the effort scaling.
    pub stream_idle_secs: Option<u64>,
    /// `max_retries` override for this provider alone.
    pub max_retries: Option<u32>,
}

impl ProviderTimeouts {
    pub fn connect_secs_or(&self, fallback: u64) -> u64 {
        self.connect_secs.unwrap_or(fallback)
    }

    pub fn stream_idle_secs_or(&self, fallback: u64) -> u64 {
        self.stream_idle_secs.unwrap_or(fallback)
    }

    pub fn max_retries_or(&self, fallback: u32) -> u32 {
        self.max_retries.unwrap_or(fallback)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub kind: ProviderKind,
    /// Explicit wire dialect. `None` = derive from `kind` (see
    /// [`ProviderDialect::from_kind`]) — the path every pre-`dialect` config
    /// takes, so `kind` keeps working unchanged.
    pub dialect: Option<ProviderDialect>,
    /// e.g. `https://api.example.com/v1`
    pub base_url: String,
    /// Request path appended to `base_url`, e.g. `/chat/completions`. The single
    /// placeholder `{model}` is substituted per request, so an endpoint that
    /// routes by model (`/v1/models/{model}/generate`) is a config, not a fork.
    /// Empty/absent = the dialect's own default path.
    pub path: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    /// Fetch `GET /models` for the picker.
    pub catalog: bool,
    pub models: Vec<ModelConfig>,
    /// Default context window for models of this provider when unknown.
    pub context_window: Option<u32>,
    /// Send `reasoning_effort`. `None` = auto (send for models flagged as reasoning).
    pub reasoning: Option<bool>,
    /// Override the wire value per effort, e.g. `{ max = "xhigh" }`.
    pub effort_map: BTreeMap<String, String>,
    /// Merged into every request body.
    pub extra_body: Option<Value>,
    pub headers: BTreeMap<String, String>,
    pub proxy: Option<String>,
    /// Model used when a route names this provider without one.
    pub default_model: Option<String>,
    /// Higher wins when several providers could serve a bare model name.
    /// Absent = 0, and ties keep the alphabetically first id, so a config
    /// without priorities resolves exactly as it did before.
    pub priority: i32,
    /// Provider ids to try when this one can no longer serve a route (removed
    /// from the config, or unusable). Never a cycle: the registry walks the
    /// chain once, in declaration order.
    pub fallbacks: Vec<String>,
    pub timeouts: ProviderTimeouts,
    pub compat: ProviderCompat,
}

impl ProviderConfig {
    /// The dialect this entry speaks: the explicit one when set, else the one
    /// the legacy `kind` names.
    pub fn effective_dialect(&self) -> ProviderDialect {
        self.dialect
            .unwrap_or_else(|| ProviderDialect::from_kind(self.kind))
    }

    /// Whether this entry names a dialect explicitly (i.e. not derived from
    /// `kind`). Hot reload compares the whole entry, so this is only a
    /// diagnostic label.
    pub fn has_explicit_dialect(&self) -> bool {
        self.dialect.is_some()
    }

    /// Completion URL for `model`.
    ///
    /// `path` is the deployment half of the endpoint: a fixed suffix
    /// (`/chat/completions`) or a template whose `{model}` placeholder is
    /// filled with the model id (URL-escaped), so a gateway that routes per
    /// model is declared rather than forked. An empty `path` falls back to the
    /// dialect's default, which is what a config written before `path` existed
    /// does.
    pub fn completion_url(&self, dialect: ProviderDialect, model: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let default = default_path(dialect);
        let path = self
            .path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(default);
        if path.contains("{model}") {
            return format!("{base}{}", path.replace("{model}", &percent_encode(model)));
        }
        format!("{base}{path}")
    }

    /// Catalog (`GET /models`) URL. Derived from `base_url` alone: the catalog
    /// lives next to the completion endpoint, and a provider that has no
    /// catalog simply never fetches one (`catalog = false`).
    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    /// The model a route `provider/` (or a bare id that lands here) means.
    pub fn default_model(&self) -> Option<&str> {
        self.default_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }
}

/// Whether `pcfg` names a usable `default_model`.
fn has_default_model(pcfg: &ProviderConfig) -> bool {
    pcfg.default_model().is_some()
}

/// The completion path a dialect uses when the config does not name one.
pub fn default_path(dialect: ProviderDialect) -> &'static str {
    match dialect {
        ProviderDialect::OpenAiCompatible => "/chat/completions",
        ProviderDialect::Anthropic => "/messages",
        ProviderDialect::Codex => "/responses",
        // Devin and OpenCode Zen speak the OpenAI shape.
        ProviderDialect::Devin | ProviderDialect::OpenCodeZen => "/chat/completions",
    }
}

/// Percent-encode the characters that would otherwise change the meaning of a
/// model id inside a URL path (slash included — a model id may contain one, as
/// on OpenRouter).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u32>,
    pub reasoning: Option<bool>,
    /// Effort levels the model supports (`efforts = ["low", "high"]`). `None`
    /// means every [`Effort`] is selectable (spec §6).
    pub efforts: Option<Vec<Effort>>,
    /// Hard cap on a single completion for this model. `None` = no cap beyond
    /// what the turn asked for.
    pub max_output_tokens: Option<u32>,
    /// Overrides the id-substring vision heuristic for this model.
    pub vision: Option<bool>,
    /// `false` hides every tool from the model (chat-only deployments).
    pub supports_tools: Option<bool>,
}

impl ModelConfig {
    /// Effective image support for this model, falling back to the provider's
    /// own guess when the entry does not say.
    pub fn vision_or(&self, guessed: bool) -> bool {
        self.vision.unwrap_or(guessed)
    }

    /// Effective tool support; a model that says nothing accepts tools.
    pub fn tools_or(&self, default: bool) -> bool {
        self.supports_tools.unwrap_or(default)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ups {
    Fixed(u16),
    Dynamic,
    Auto,
}

impl Default for Ups {
    fn default() -> Self {
        Self::Fixed(10)
    }
}

impl Ups {
    pub fn frame_interval(&self, detected_hz: Option<u16>) -> Option<Duration> {
        match *self {
            Self::Dynamic => None,
            Self::Fixed(n) => {
                let clamped = n.clamp(1, 240);
                Some(Duration::from_millis(1000 / clamped as u64))
            }
            Self::Auto => {
                let hz = detected_hz.unwrap_or(10);
                let clamped = hz.clamp(1, 240);
                Some(Duration::from_millis(1000 / clamped as u64))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum UpsHelper {
    Fixed(u16),
    Named(UpsNamed),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UpsNamed {
    Dynamic,
    Auto,
}

impl Serialize for Ups {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let helper = match *self {
            Self::Fixed(n) => UpsHelper::Fixed(n),
            Self::Dynamic => UpsHelper::Named(UpsNamed::Dynamic),
            Self::Auto => UpsHelper::Named(UpsNamed::Auto),
        };
        helper.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Ups {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let helper = UpsHelper::deserialize(deserializer)?;
        let ups = match helper {
            UpsHelper::Fixed(n) => Self::Fixed(n),
            UpsHelper::Named(UpsNamed::Dynamic) => Self::Dynamic,
            UpsHelper::Named(UpsNamed::Auto) => Self::Auto,
        };
        Ok(ups)
    }
}

/// pacode's own plugin marketplace.
fn default_marketplace() -> String {
    "tridtsatx/pacode-plugins".to_string()
}

fn default_images() -> String {
    "auto".to_string()
}

fn default_tool_memory_split() -> bool {
    true
}

fn default_tool_memory_percent() -> u8 {
    TOOL_MEMORY_DEFAULT_PERCENT
}

fn default_analyze_checkpoint_every() -> u32 {
    ANALYZE_CHECKPOINT_DEFAULT
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub hints: UiHints,
    pub ascii_only: bool,
    /// The terminal font has Powerline/Nerd Font glyphs: draw arrow
    /// separators in the status line.
    pub powerline: bool,
    pub mouse: bool,
    pub auto_copy: bool,
    #[serde(default)]
    pub ups: Ups,
    /// `auto` (COLORTERM detection), `truecolor`, or `ansi`.
    pub color: String,
    /// Max transcript cells kept in the client before older ones are evicted.
    pub transcript_cells: usize,
    /// `auto` or `off`.
    #[serde(default = "default_images")]
    pub images: String,
    /// Vim keybindings for prompt input (`[ui] vim = true`).
    #[serde(default)]
    pub vim: bool,
    /// Show the model's reasoning line in the transcript. Off by default: the
    /// reasoning is still recorded, it is just not drawn.
    #[serde(default)]
    pub thinking: bool,
    /// Where a selected subagent's conversation is shown.
    #[serde(default)]
    pub agent_view: AgentView,
    /// Animation clock preset (`smooth` / `balanced` / `eco`): the cadence of
    /// the spinner/pacman tick and of short transient animations, independent
    /// of the content frame rate (`ups`).
    #[serde(default)]
    pub animation: AnimationMode,
    /// Sweep animation on live text (`classic` / `kitt` / `off`): a light band
    /// travelling across the activity label and running tool status.
    #[serde(default)]
    pub shimmer: ShimmerMode,
    /// Ceiling for one tool process, in percent of machine RAM. `0` disables
    /// the ceiling entirely. The runtime is the authority on what it can
    /// actually enforce; this is the number it is given.
    #[serde(default = "default_tool_memory_percent")]
    pub tool_memory_percent: u8,
    /// Split the ceiling between concurrent sessions instead of applying it to
    /// each one: two sessions get half each, so the machine as a whole stays
    /// inside the ceiling.
    ///
    /// The default is named rather than `#[serde(default)]`: a bare
    /// `#[serde(default)]` on a `bool` field resolves to `false`, not to the
    /// container's default, so a config that omits the key would silently
    /// un-split the ceiling.
    #[serde(default = "default_tool_memory_split")]
    pub tool_memory_split_sessions: bool,
    /// Planner/implementer pair. `None` = no fusion: one model per turn.
    #[serde(default)]
    pub fusion: Option<FusionConfig>,
    /// Actions between context-quality checkpoints, for `--analyze-mode`.
    #[serde(default = "default_analyze_checkpoint_every")]
    pub analyze_checkpoint_every: u32,
}

/// Where a selected subagent's conversation goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentView {
    /// The subagent takes over the conversation column; the main thread comes
    /// back when the subagent is deselected. The default: on a normal terminal
    /// a split column leaves neither conversation readable.
    #[default]
    Replace,
    /// The subagent opens beside the main conversation, splitting the column.
    Panel,
}

impl AgentView {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Panel => "panel",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "replace" => Some(Self::Replace),
            "panel" => Some(Self::Panel),
            _ => None,
        }
    }
}

/// `[ui] animation`: the preset driving the animation clock, a timer fully
/// separate from the content frame clock (`ups`). `tick_ms` paces the
/// spinner/pacman while activity runs; `fast_tick_ms` paces short transient
/// animations (toast slide/fade, overlay unfold, welcome cascade) — honest
/// 60 fps in the default `smooth` mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnimationMode {
    /// Spinner at ~15 fps, transient animations at ~60 fps.
    #[default]
    Smooth,
    /// Spinner at ~10 fps, transient animations at ~30 fps.
    Balanced,
    /// Spinner at ~4 fps, transient animations at ~14 fps.
    Eco,
}

impl AnimationMode {
    /// The spinner/pacman cadence while animation is active.
    pub fn tick_ms(self) -> u64 {
        match self {
            Self::Smooth => 66,
            Self::Balanced => 100,
            Self::Eco => 240,
        }
    }

    /// The cadence while a short transient animation (overlay unfold, welcome
    /// cascade, toast slide/fade) is on screen.
    pub fn fast_tick_ms(self) -> u64 {
        match self {
            Self::Smooth => 16,
            Self::Balanced => 33,
            Self::Eco => 70,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::Balanced => "balanced",
            Self::Eco => "eco",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "smooth" => Some(Self::Smooth),
            "balanced" => Some(Self::Balanced),
            "eco" => Some(Self::Eco),
            _ => None,
        }
    }
}

/// `[ui] shimmer`: the sweep drawn across *live* text — the activity label and
/// a running tool's status. A sweep is a pure function of elapsed animation
/// time, so a degraded cadence drops band positions instead of slowing the
/// band down, exactly like every other animation in the client.
///
/// `off` is a first-class choice, not a fallback: an animated band is motion on
/// a screen some people read for hours, and `off` must cost nothing at all —
/// not even the per-character walk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShimmerMode {
    /// A cosine band sweeping left to right and wrapping, with padding so the
    /// band fades in off-screen and never pops at the text's first cell.
    #[default]
    Classic,
    /// A Knight Rider scanner: one bright head ping-ponging across the text
    /// with a quadratic trail behind it and no leading glow.
    Kitt,
    /// No sweep; live text is drawn in one flat colour.
    Off,
}

impl ShimmerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Kitt => "kitt",
            Self::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "classic" => Some(Self::Classic),
            "kitt" | "kittt" => Some(Self::Kitt),
            "off" | "none" | "disabled" => Some(Self::Off),
            _ => None,
        }
    }

    /// Whether a sweep is drawn at all.
    pub fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// `[ui] fusion`: a second, cheaper model that implements what the primary one
/// planned. A pipeline, not a preference: the planner reads the repository and
/// returns a plan, the implementer turns that plan into edits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionConfig {
    /// Route for the model that thinks — `provider/model`.
    pub planner: String,
    /// Route for the model that implements, `provider/model`.
    pub implementer: String,
}

/// Lowest and highest `[ui] tool_memory` accepts, in percent of machine RAM.
/// Below the floor a ceiling stops being a safety limit and starts being an
/// outage; above the ceiling there is nothing left to protect.
pub const TOOL_MEMORY_MIN_PERCENT: u8 = 5;
pub const TOOL_MEMORY_MAX_PERCENT: u8 = 100;

/// Default ceiling for a tool process, as a percent of machine RAM. `0` means
/// no ceiling at all — the setting exists to stop one runaway script from
/// taking the machine down with an OOM kill, not to make small machines
/// unusable.
pub const TOOL_MEMORY_DEFAULT_PERCENT: u8 = 80;

/// Default number of actions between context-quality checkpoints.
pub const ANALYZE_CHECKPOINT_DEFAULT: u32 = 100;

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            hints: UiHints::default(),
            ascii_only: false,
            powerline: false,
            mouse: true,
            auto_copy: true,
            ups: Ups::default(),
            color: "auto".to_string(),
            transcript_cells: 500,
            images: default_images(),
            vim: false,
            thinking: false,
            agent_view: AgentView::default(),
            animation: AnimationMode::default(),
            shimmer: ShimmerMode::default(),
            tool_memory_percent: default_tool_memory_percent(),
            tool_memory_split_sessions: true,
            fusion: None,
            analyze_checkpoint_every: default_analyze_checkpoint_every(),
        }
    }
}

impl UiConfig {
    pub fn images_enabled(&self) -> bool {
        self.images != "off"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiHints {
    pub model: bool,
    pub effort: bool,
}

impl Default for UiHints {
    fn default() -> Self {
        Self {
            model: false,
            effort: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecConfig {
    /// Foreground wait before a command is handed to the background.
    pub yield_after_secs: u64,
    /// Silence before a stall notification.
    pub stall_secs: u64,
    pub max_spool_bytes: u64,
    /// In-RAM tail per task.
    pub tail_bytes: usize,
    pub kill_grace_secs: u64,
    /// Simultaneous processes one session may run. Unlike the per-agent tool
    /// worker cap this is a real OS budget — every task holds a live process
    /// group, three descriptors and ~80 KiB of RAM — so it is a ceiling on
    /// concurrency, not a queue length. It does bind a fleet: 128 subagents that
    /// all reach for `bash` at once will hit it, and the rest get
    /// `TooManyTasks` rather than waiting. Raise it for such a session.
    pub max_tasks: usize,
    /// Hard cap for a foreground command without `background`.
    pub default_timeout_secs: u64,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            yield_after_secs: 5,
            stall_secs: 120,
            max_spool_bytes: 50 * 1024 * 1024,
            tail_bytes: 64 * 1024,
            kill_grace_secs: 5,
            max_tasks: 64,
            default_timeout_secs: 3600,
        }
    }
}

/// Default and hard ceiling for `limits.max_provider_concurrency`. The values
/// mirror the provider runtime's own governor (`DEFAULT_PROVIDER_CONCURRENCY`
/// / `MAX_PROVIDER_CONCURRENCY`), which rejects anything outside them — the
/// daemon hands the config value to `set_provider_concurrency_ceiling` and
/// keeps the runtime's answer.
pub const DEFAULT_PROVIDER_CONCURRENCY: u32 = 128;
pub const MAX_PROVIDER_CONCURRENCY: u32 = 1024;

/// `[limits]`: the ceilings the runtime really enforces. Every field here is
/// applied by `pacode-daemon`'s wiring at startup; a limit nothing reads would
/// be a lie, so there are none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// In-flight provider requests allowed per (base url, credential)
    /// governor; the daemon sets it as the runtime ceiling.
    pub max_provider_concurrency: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_provider_concurrency: DEFAULT_PROVIDER_CONCURRENCY,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    #[serde(skip)]
    pub default_model: Option<String>,
    #[serde(skip)]
    pub default_effort: Option<Effort>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub idle_timeout_secs: u64,
    pub socket: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: 600,
            socket: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Fraction of the context window that triggers compaction.
    pub compaction_threshold: f32,
    pub tool_output_cap_chars: usize,
    pub injection_cap_chars: usize,
    /// Cap for AGENTS.md / CLAUDE.md content in the system prompt.
    pub instructions_cap_chars: usize,
    pub memory_cap_chars: usize,
    pub compaction_model: Option<String>,
    /// Recent messages kept verbatim after compaction.
    pub keep_recent_messages: usize,
    /// Used when neither the provider nor the config knows the window.
    pub default_context_window: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            compaction_threshold: 0.85,
            tool_output_cap_chars: 16_000,
            injection_cap_chars: 4_000,
            instructions_cap_chars: 32_000,
            memory_cap_chars: 8_000,
            compaction_model: None,
            keep_recent_messages: 6,
            default_context_window: 128_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    pub default_mode: Mode,
    pub allow_catastrophic: bool,
    /// `provider/model` for permission explanations (the `e` key on a pending
    /// permission); default: the session's own model.
    pub explain_model: Option<String>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default_mode: Mode::Build,
            allow_catastrophic: false,
            explain_model: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub servers: BTreeMap<String, McpServerConfig>,
    pub sampling: bool,
    pub sampling_max_tokens: u32,
    pub idle_timeout_secs: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            servers: BTreeMap::new(),
            sampling: true,
            sampling_max_tokens: 2048,
            idle_timeout_secs: 300,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub enabled: bool,
    /// Start on first tool call instead of at session start.
    pub lazy: bool,
    pub timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            enabled: true,
            lazy: true,
            timeout_secs: 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Model used to name the session after the first reply; default: session model.
    pub title_model: Option<String>,
    /// Transcript items sent in a snapshot / history page.
    pub history_page: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            title_model: None,
            history_page: 200,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginsConfig {
    pub enabled: bool,
    /// Empty means default `~/.config/pacode/plugins`.
    pub dirs: Vec<PathBuf>,
    pub wasm_memory_mb: u32,
    pub lua_memory_mb: u32,
    pub hook_timeout_ms: u64,
    /// Marketplace to browse in `/plugins`: `owner/repo`, `owner/repo@ref` or an
    /// https URL. Empty means none.
    #[serde(default = "default_marketplace")]
    pub marketplace: String,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            marketplace: default_marketplace(),
            enabled: true,
            dirs: Vec::new(),
            wasm_memory_mb: 64,
            lua_memory_mb: 32,
            hook_timeout_ms: 5000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub enabled: bool,
    /// Empty means default `<config dir>/skills`.
    pub dirs: Vec<PathBuf>,
    pub max_body_bytes: usize,
    pub max_listed: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dirs: Vec::new(),
            max_body_bytes: 16384,
            max_listed: 100,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Default number of results returned by websearch if unspecified.
    pub default_num_results: usize,
    /// Request timeout in seconds for web tools.
    pub request_timeout_secs: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            default_num_results: 8,
            request_timeout_secs: 15,
        }
    }
}

/// `[hooks]`: user shell commands fired on tool/lifecycle events, Claude-Code
/// style. Each matching rule runs `sh -c <command>` with the event context in
/// `PACODE_HOOK_*` env vars and a JSON payload on stdin. Absent means no hooks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Run before a tool call: exit code 2 blocks the call and the hook's
    /// stderr goes back to the model as the tool error; any other nonzero exit
    /// only warns. Timeouts and spawn failures are fail-open.
    pub pre_tool_use: Vec<HookRule>,
    /// Run after a tool call: a nonzero exit surfaces stderr as a notice.
    /// Never blocks.
    pub post_tool_use: Vec<HookRule>,
    /// Fired when a session starts (lazily, on its first turn).
    pub session_start: Vec<HookRule>,
    /// Fired when a session ends.
    pub session_end: Vec<HookRule>,
    /// Fired on permission requests and user-question prompts.
    pub notification: Vec<HookRule>,
}

/// One `[[hooks.<event>]]` rule: run `command` when `matcher` accepts the tool
/// name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HookRule {
    /// Regex matched against the tool name; empty or `.*` matches everything.
    pub matcher: String,
    /// Shell command run via `sh -c`.
    pub command: String,
    /// Kill the hook after this many seconds; a timed-out `pre_tool_use` hook
    /// allows the call (fail-open).
    pub timeout_secs: u64,
}

impl Default for HookRule {
    fn default() -> Self {
        Self {
            matcher: String::new(),
            command: String::new(),
            timeout_secs: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_gives_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.exec.yield_after_secs, 5);
        assert_eq!(cfg.limits.max_provider_concurrency, 128);
        assert_eq!(cfg.provider.catalog_ttl_secs, 86400);
        assert!(cfg.provider.proxy.is_none());
        assert_eq!(cfg.permissions.default_mode, Mode::Build);
        assert!(!cfg.ui.hints.model);
        assert_eq!(cfg.ui.ups, Ups::Fixed(10));
        assert_eq!(cfg.ui.animation, AnimationMode::Smooth);
        assert_eq!(cfg.ui.images, "auto");
        assert!(cfg.ui.images_enabled());
        assert!(!cfg.ui.vim);
        assert!(cfg.skills.enabled);
        assert!(cfg.skills.dirs.is_empty());
        assert_eq!(cfg.skills.max_body_bytes, 16384);
        assert_eq!(cfg.ui.animation, AnimationMode::Smooth);
        // Named serde defaults, checked through a *parse*: a bare
        // `#[serde(default)]` on these would resolve to the field type's zero
        // and never reach the container's default, which is how a configured
        // ceiling silently stopped being split.
        assert_eq!(cfg.ui.shimmer, ShimmerMode::Classic);
        assert_eq!(cfg.ui.tool_memory_percent, TOOL_MEMORY_DEFAULT_PERCENT);
        assert!(cfg.ui.tool_memory_split_sessions);
        assert_eq!(cfg.ui.analyze_checkpoint_every, ANALYZE_CHECKPOINT_DEFAULT);
        assert!(cfg.ui.fusion.is_none());
        assert_eq!(cfg.web.default_num_results, 8);
        assert_eq!(cfg.web.request_timeout_secs, 15);
        assert_eq!(cfg.mcp.idle_timeout_secs, 300);
        assert!(cfg.default_route().is_none());
        assert!(cfg.hooks.pre_tool_use.is_empty());
        assert!(cfg.hooks.post_tool_use.is_empty());
        assert!(cfg.hooks.session_start.is_empty());
        assert!(cfg.hooks.session_end.is_empty());
        assert!(cfg.hooks.notification.is_empty());
        assert!(cfg.tools.is_empty());
    }

    #[test]
    fn removed_limits_are_ignored_rather_than_fatal() {
        // Every key the old `[limits]` table carried is gone from the schema.
        // An old config.toml must still load, whatever it put there.
        let stale: Config = serde_json::from_value(serde_json::json!({
            "limits": {
                "max_logical_agents": "many",
                "max_provider_queue_items": 8192,
                "max_provider_queue_bytes": 134217728,
                "max_exec_calls_per_turn": 64,
                "max_tool_calls_per_turn": 512,
            }
        }))
        .expect("unknown [limits] keys must not fail deserialization");
        assert_eq!(stale.limits, LimitsConfig::default());

        // A stale concurrency value out of the runtime's range is kept as read
        // here; the load path (`pacode-config`) is what clamps it.
        let stale: Config = serde_json::from_value(
            serde_json::json!({ "limits": { "max_provider_concurrency": 4096 } }),
        )
        .expect("out-of-range concurrency must still deserialize");
        assert_eq!(stale.limits.max_provider_concurrency, 4096);
    }

    #[test]
    fn tool_toggles_parse_and_query() {
        // Same shape as `[tools.bash] enabled = false` in config.toml.
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "tools": {
                "bash": { "enabled": false },
                "github__get_pr": { "enabled": false },
                "read": {},
            }
        }))
        .unwrap();
        assert!(!cfg.tool_enabled("bash"));
        assert!(!cfg.tool_enabled("github__get_pr"));
        // A present-but-empty table means enabled; absent tools are enabled.
        assert!(cfg.tool_enabled("read"));
        assert!(cfg.tool_enabled("grep"));
        let mut disabled: Vec<&str> = cfg.disabled_tool_names().collect();
        disabled.sort();
        assert_eq!(disabled, ["bash", "github__get_pr"]);
    }

    #[test]
    fn hooks_config_parses() {
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "hooks": {
                "pre_tool_use": [
                    { "matcher": "bash|edit", "command": "./check.sh", "timeout_secs": 5 },
                    { "command": "echo all" }
                ],
                "notification": [{ "matcher": ".*", "command": "notify-send hi" }]
            }
        }))
        .unwrap();
        assert_eq!(cfg.hooks.pre_tool_use.len(), 2);
        assert_eq!(cfg.hooks.pre_tool_use[0].matcher, "bash|edit");
        assert_eq!(cfg.hooks.pre_tool_use[0].command, "./check.sh");
        assert_eq!(cfg.hooks.pre_tool_use[0].timeout_secs, 5);
        // Absent fields take the HookRule defaults.
        assert_eq!(cfg.hooks.pre_tool_use[1].matcher, "");
        assert_eq!(cfg.hooks.pre_tool_use[1].timeout_secs, 10);
        assert_eq!(cfg.hooks.notification.len(), 1);
    }

    #[test]
    fn proxy_config_serde() {
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "provider": { "proxy": "http://global-proxy:8080" },
            "providers": {
                "custom": { "base_url": "https://custom.api/v1", "proxy": "socks5://127.0.0.1:1080" },
                "direct": { "base_url": "https://direct.api/v1", "proxy": "none" },
                "defaulted": { "base_url": "https://defaulted.api/v1" }
            }
        }))
        .unwrap();
        assert_eq!(
            cfg.provider.proxy.as_deref(),
            Some("http://global-proxy:8080")
        );
        assert_eq!(
            cfg.providers["custom"].proxy.as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
        assert_eq!(cfg.providers["direct"].proxy.as_deref(), Some("none"));
        assert_eq!(cfg.providers["defaulted"].proxy, None);
    }

    #[test]
    fn ui_images_off() {
        let cfg: Config = serde_json::from_str(r#"{"ui": {"images": "off"}}"#).unwrap();
        assert_eq!(cfg.ui.images, "off");
        assert!(!cfg.ui.images_enabled());
    }

    #[test]
    fn default_route_uses_known_providers() {
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "provider": { "default": "bubna/gemini-3.8-flash" },
            "providers": { "bubna": { "base_url": "https://x/v1" } }
        }))
        .unwrap();
        let route = cfg.default_route().unwrap();
        assert_eq!(route.provider, "bubna");
        assert_eq!(route.model, "gemini-3.8-flash");
    }

    #[test]
    fn ups_serde_and_clamping() {
        let fixed: Ups = serde_json::from_str("10").unwrap();
        assert_eq!(fixed, Ups::Fixed(10));
        assert_eq!(fixed.frame_interval(None), Some(Duration::from_millis(100)));

        let dyn_val: Ups = serde_json::from_str("\"dynamic\"").unwrap();
        assert_eq!(dyn_val, Ups::Dynamic);
        assert_eq!(dyn_val.frame_interval(Some(60)), None);

        let auto_val: Ups = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(auto_val, Ups::Auto);
        assert_eq!(
            auto_val.frame_interval(None),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            auto_val.frame_interval(Some(60)),
            Some(Duration::from_millis(16))
        );

        // Clamping tests
        assert_eq!(
            Ups::Fixed(0).frame_interval(None),
            Some(Duration::from_millis(1000))
        );
        assert_eq!(
            Ups::Fixed(300).frame_interval(None),
            Some(Duration::from_millis(4))
        );
        assert_eq!(
            Ups::Auto.frame_interval(Some(0)),
            Some(Duration::from_millis(1000))
        );
        assert_eq!(
            Ups::Auto.frame_interval(Some(300)),
            Some(Duration::from_millis(4))
        );

        // Symmetric serialization
        assert_eq!(serde_json::to_string(&Ups::Fixed(10)).unwrap(), "10");
        assert_eq!(serde_json::to_string(&Ups::Dynamic).unwrap(), "\"dynamic\"");
        assert_eq!(serde_json::to_string(&Ups::Auto).unwrap(), "\"auto\"");
    }

    #[test]
    fn animation_mode_serde_and_ticks() {
        // `smooth` is the default both for a missing key and a missing table.
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.ui.animation, AnimationMode::Smooth);
        assert_eq!(AnimationMode::default(), AnimationMode::Smooth);

        for (raw, want, tick, fast) in [
            ("smooth", AnimationMode::Smooth, 66, 16),
            ("balanced", AnimationMode::Balanced, 100, 33),
            ("eco", AnimationMode::Eco, 240, 70),
        ] {
            let cfg: Config =
                serde_json::from_str(&format!("{{\"ui\": {{\"animation\": \"{raw}\"}}}}")).unwrap();
            assert_eq!(cfg.ui.animation, want);
            assert_eq!(want.as_str(), raw);
            assert_eq!(AnimationMode::parse(raw), Some(want));
            assert_eq!(want.tick_ms(), tick);
            assert_eq!(want.fast_tick_ms(), fast);
            assert_eq!(serde_json::to_string(&want).unwrap(), format!("\"{raw}\""));
        }

        assert_eq!(AnimationMode::parse("wobbly"), None);
        assert!(serde_json::from_str::<AnimationMode>("\"wobbly\"").is_err());
    }

    #[test]
    fn zdr_mode_serde() {
        // `session` is the default both for a missing key and a missing table.
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.zdr, ZdrMode::Session);
        assert_eq!(ZdrMode::default(), ZdrMode::Session);

        for (raw, want) in [
            ("always", ZdrMode::Always),
            ("session", ZdrMode::Session),
            ("off", ZdrMode::Off),
        ] {
            let cfg: Config = serde_json::from_str(&format!("{{\"zdr\": \"{raw}\"}}")).unwrap();
            assert_eq!(cfg.zdr, want);
            assert_eq!(serde_json::to_string(&want).unwrap(), format!("\"{raw}\""));
        }

        assert!(serde_json::from_str::<ZdrMode>("\"sometimes\"").is_err());
    }
}

/// `[theme]`: which palette to use. `name` is a built-in id or the stem of a
/// `<config dir>/themes/<name>.toml` file; `overrides` patches individual roles
/// on top of it, so a user can tweak one colour without copying a whole theme.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub name: String,
    /// Role name (`accent`, `green`, ...) to colour (`#rrggbb`, `red`, `9`).
    pub overrides: BTreeMap<String, String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "pacode-dark".to_string(),
            overrides: BTreeMap::new(),
        }
    }
}

/// `[font]`: applied best effort. A terminal application does not own its font;
/// only some emulators expose a control sequence for it, so every field here is
/// a request that may be silently unavailable.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    pub family: Option<String>,
    pub size: Option<u16>,
    /// `normal` or `bold`; only affects what pacode draws itself.
    pub weight: Option<String>,
}

/// `[keys]`: action name to key binding, e.g. `follow_agent = "alt+f"`, plus
/// the double-Shift hub settings. Unset actions keep their built-in binding.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Double-tap Shift opens the menu hub (default true; §4.2 of the menu
    /// spec). Named field — not a binding — so `Keymap::from_config` never
    /// sees it as an unknown action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_shift: Option<bool>,
    /// Max milliseconds between the two Shift taps (default 400).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_shift_ms: Option<u64>,
    #[serde(flatten)]
    pub bindings: BTreeMap<String, String>,
}

impl KeysConfig {
    /// Whether the double-Shift gesture opens the menu hub.
    pub fn double_shift_enabled(&self) -> bool {
        self.double_shift.unwrap_or(true)
    }

    /// The pair window in milliseconds.
    pub fn double_shift_threshold_ms(&self) -> u64 {
        self.double_shift_ms.unwrap_or(400)
    }
}
