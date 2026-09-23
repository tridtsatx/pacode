//! ACP session config options: the mode/model/effort selectors and the ZDR
//! toggle that clients like Zed render in the agent panel.
//!
//! When a session response carries `config_options`, Zed ignores `modes`
//! entirely and renders each option as a picker — so the mode selector must
//! be re-emitted here as a select option or it disappears from the UI.
//! Options are only sent to clients that advertised `session.config_options`
//! at initialize (`ConfigOptionCaps`); others keep the legacy `modes` field.

use agent_client_protocol::schema::v1::{
    ClientCapabilities, SessionConfigOption, SessionConfigOptionCategory, SessionConfigOptionValue,
    SessionConfigSelectGroup, SessionConfigSelectOption,
};
use pacode_types::model::{Effort, ModelInfo, ModelRoute};
use pacode_types::protocol::{Event, Request};
use pacode_types::state::{Mode, SessionMeta};

use crate::error::AcpError;
use crate::mapping::mode_description;

/// Config option ids emitted in `config_options` and accepted by
/// `session/set_config_option`.
const MODE_OPTION: &str = "mode";
const MODEL_OPTION: &str = "model";
const EFFORT_OPTION: &str = "effort";
const ZDR_OPTION: &str = "zdr";

/// Which session-config-option features the client advertised at initialize.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfigOptionCaps {
    /// `session.config_options` present: select options, `config_option_update`
    /// notifications and `session/set_config_option` are supported.
    pub select: bool,
    /// `config_options.boolean` present: `type: "boolean"` options render.
    /// Without it the ZDR toggle degrades to a two-value select.
    pub boolean: bool,
}

/// Reads the config-option capabilities out of the client's initialize caps.
pub fn config_options_caps(caps: &ClientCapabilities) -> ConfigOptionCaps {
    let config = caps
        .session
        .as_ref()
        .and_then(|session| session.config_options.as_ref());
    ConfigOptionCaps {
        select: config.is_some(),
        boolean: config.is_some_and(|config| config.boolean.is_some()),
    }
}

/// State backing the config options: the last known session meta and the
/// model catalog, refreshed on `SessionUpdated`/`ModelsUpdated` events.
#[derive(Clone, Debug)]
pub struct SessionConfigState {
    pub meta: SessionMeta,
    pub models: Vec<ModelInfo>,
}

/// Builds the full config-option list for a session. Order is stable —
/// mode, model, effort, zdr — matching Zed's top-to-bottom picker display.
pub fn session_config_options(
    meta: &SessionMeta,
    models: &[ModelInfo],
    caps: ConfigOptionCaps,
) -> Vec<SessionConfigOption> {
    vec![
        mode_option(meta.mode),
        model_option(&meta.model, models),
        effort_option(meta, models),
        zdr_option(meta.zdr, caps.boolean),
    ]
}

/// Folds a daemon event into the cached config state; returns `true` when the
/// cache changed and the option list should be re-emitted to the client.
pub fn apply_event(state: &mut SessionConfigState, event: &Event) -> bool {
    if let Event::SessionUpdated(meta) = event {
        state.meta = meta.clone();
        true
    } else if let Event::ModelsUpdated { models } = event {
        state.models.clone_from(models);
        true
    } else {
        false
    }
}

/// A validated config-option change, ready to send to the daemon and to fold
/// into the cached meta once acknowledged.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigChange {
    Mode(Mode),
    Model(ModelRoute),
    Effort(Effort),
    Zdr(bool),
}

impl ConfigChange {
    /// The daemon request carrying this change.
    pub fn to_request(&self) -> Request {
        match self {
            ConfigChange::Mode(mode) => Request::SetMode(*mode),
            ConfigChange::Model(route) => Request::SetModel(route.clone()),
            ConfigChange::Effort(effort) => Request::SetEffort(*effort),
            ConfigChange::Zdr(zdr) => Request::SetZdr(*zdr),
        }
    }

    /// Applies an acknowledged change to a cached `SessionMeta`.
    pub fn apply_to_meta(&self, meta: &mut SessionMeta) {
        match self {
            ConfigChange::Mode(mode) => meta.mode = *mode,
            ConfigChange::Model(route) => meta.model = route.clone(),
            ConfigChange::Effort(effort) => meta.effort = *effort,
            ConfigChange::Zdr(zdr) => meta.zdr = *zdr,
        }
    }
}

/// Maps a `session/set_config_option` value to a validated change. Unknown
/// option ids and malformed values are `Internal` errors, not silent drops.
pub fn apply_config_value(
    config_id: &str,
    value: &SessionConfigOptionValue,
) -> Result<ConfigChange, AcpError> {
    match config_id {
        MODE_OPTION => {
            let id = require_value_id(config_id, value)?;
            let mode = Mode::parse(id)
                .ok_or_else(|| AcpError::Internal(format!("unknown mode value: {id}")))?;
            Ok(ConfigChange::Mode(mode))
        }
        MODEL_OPTION => {
            let id = require_value_id(config_id, value)?;
            let route = ModelRoute::parse_lossy(id)
                .ok_or_else(|| AcpError::Internal(format!("invalid model route: {id}")))?;
            Ok(ConfigChange::Model(route))
        }
        EFFORT_OPTION => {
            let id = require_value_id(config_id, value)?;
            let effort = Effort::parse(id)
                .ok_or_else(|| AcpError::Internal(format!("unknown effort value: {id}")))?;
            Ok(ConfigChange::Effort(effort))
        }
        ZDR_OPTION => zdr_value(value).map(ConfigChange::Zdr),
        other => Err(AcpError::Internal(format!(
            "unknown config option: {other}"
        ))),
    }
}

/// Extracts a select-style value id; other value shapes are a client error.
fn require_value_id<'a>(
    config_id: &str,
    value: &'a SessionConfigOptionValue,
) -> Result<&'a str, AcpError> {
    value.as_value_id().map(|id| id.0.as_ref()).ok_or_else(|| {
        AcpError::Internal(format!(
            "config option '{config_id}' expects a select value id"
        ))
    })
}

/// Boolean options arrive as `Boolean` from spec-compliant clients; the
/// `"true"`/`"false"` value ids of the select fallback are accepted too.
fn zdr_value(value: &SessionConfigOptionValue) -> Result<bool, AcpError> {
    match value {
        SessionConfigOptionValue::Boolean { value } => Ok(*value),
        SessionConfigOptionValue::ValueId { value } => match value.0.as_ref() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(AcpError::Internal(format!("invalid zdr value: {other}"))),
        },
        // `SessionConfigOptionValue` is non_exhaustive; future value kinds
        // cannot map to a bool.
        _ => Err(AcpError::Internal(
            "unsupported value type for boolean option".to_string(),
        )),
    }
}

fn mode_option(current: Mode) -> SessionConfigOption {
    let options: Vec<SessionConfigSelectOption> = Mode::CYCLE
        .iter()
        .map(|mode| {
            SessionConfigSelectOption::new(mode.as_str(), mode.label())
                .description(mode_description(*mode))
        })
        .collect();
    SessionConfigOption::select(MODE_OPTION, "Mode", current.as_str(), options)
        .category(SessionConfigOptionCategory::Mode)
}

fn model_option(current: &ModelRoute, models: &[ModelInfo]) -> SessionConfigOption {
    // Group by provider, preserving the catalog's first-seen order.
    let mut groups: Vec<(String, Vec<SessionConfigSelectOption>)> = Vec::new();
    for info in models {
        let option =
            SessionConfigSelectOption::new(info.route.to_string(), info.display_name.clone())
                .description(
                    info.context_window
                        .map(|window| format!("{}k ctx", window / 1000)),
                );
        push_group(&mut groups, &info.route.provider, option);
    }
    // The current route must always be selectable: on an empty or failed
    // catalog fetch, or a model dropped from the list, this keeps the picker
    // non-empty and pointing at a real value.
    if !models.iter().any(|info| info.route == *current) {
        let option = SessionConfigSelectOption::new(current.to_string(), current.display_name());
        push_group(&mut groups, &current.provider, option);
    }
    let groups: Vec<SessionConfigSelectGroup> = groups
        .into_iter()
        .map(|(provider, options)| {
            SessionConfigSelectGroup::new(provider.clone(), provider, options)
        })
        .collect();
    SessionConfigOption::select(MODEL_OPTION, "Model", current.to_string(), groups)
        .category(SessionConfigOptionCategory::Model)
}

/// Adds `option` to `provider`'s group, creating the group on first sight.
fn push_group(
    groups: &mut Vec<(String, Vec<SessionConfigSelectOption>)>,
    provider: &str,
    option: SessionConfigSelectOption,
) {
    match groups.iter_mut().find(|(group, _)| group == provider) {
        Some((_, options)) => options.push(option),
        None => groups.push((provider.to_string(), vec![option])),
    }
}

fn effort_option(meta: &SessionMeta, models: &[ModelInfo]) -> SessionConfigOption {
    let mut efforts: Vec<Effort> = models
        .iter()
        .find(|info| info.route == meta.model)
        .and_then(|info| info.supported_efforts.clone())
        .unwrap_or_else(|| Effort::ALL.to_vec());
    // A current effort outside the model's list (stale meta, catalog change)
    // is still appended so the picker never points at a missing value.
    if !efforts.contains(&meta.effort) {
        efforts.push(meta.effort);
        efforts.sort();
    }
    let options: Vec<SessionConfigSelectOption> = efforts
        .iter()
        .map(|effort| SessionConfigSelectOption::new(effort.as_str(), effort_label(*effort)))
        .collect();
    SessionConfigOption::select(EFFORT_OPTION, "Effort", meta.effort.as_str(), options)
        .category(SessionConfigOptionCategory::ThoughtLevel)
}

fn effort_label(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "Low",
        Effort::Medium => "Medium",
        Effort::High => "High",
        Effort::XHigh => "XHigh",
        Effort::Max => "Max",
    }
}

fn zdr_option(current: bool, boolean_supported: bool) -> SessionConfigOption {
    const DESCRIPTION: &str =
        "block network tools, MCP and subagents; restrict models to zdr-capable routes";
    let option = if boolean_supported {
        SessionConfigOption::boolean(ZDR_OPTION, "Zero Data Retention", current)
    } else {
        // Clients without boolean support still get a working toggle as a
        // two-value select; `apply_config_value` accepts the same value ids.
        let options = vec![
            SessionConfigSelectOption::new("false", "Off"),
            SessionConfigSelectOption::new("true", "On"),
        ];
        SessionConfigOption::select(
            ZDR_OPTION,
            "Zero Data Retention",
            if current { "true" } else { "false" },
            options,
        )
    };
    option.description(DESCRIPTION)
}

#[cfg(test)]
#[path = "config_options_tests.rs"]
mod config_options_tests;
