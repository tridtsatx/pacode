use std::path::PathBuf;

use agent_client_protocol::schema::v1::{SessionConfigKind, SessionConfigSelectOptions};
use pacode_types::ids::SessionId as PacodeSessionId;

use super::*;

fn meta() -> SessionMeta {
    SessionMeta {
        id: PacodeSessionId::new("ses_1"),
        name: None,
        cwd: PathBuf::from("/tmp"),
        git_branch: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        model: ModelRoute::new("bubna", "gemini-3.8-flash"),
        effort: Effort::High,
        mode: Mode::Build,
        first_prompt: None,
        zdr: false,
    }
}

fn model(
    provider: &str,
    name: &str,
    window: Option<u32>,
    efforts: Option<Vec<Effort>>,
) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider, name),
        display_name: name.to_string(),
        context_window: window,
        supports_reasoning: true,
        supports_vision: true,
        supported_efforts: efforts,
        pricing: None,
    }
}

fn caps() -> ConfigOptionCaps {
    ConfigOptionCaps {
        select: true,
        boolean: true,
    }
}

fn find_option<'a>(options: &'a [SessionConfigOption], id: &str) -> &'a SessionConfigOption {
    options
        .iter()
        .find(|option| option.id.0.as_ref() == id)
        .unwrap_or_else(|| panic!("missing config option '{id}'"))
}

fn select_values(option: &SessionConfigOption) -> Vec<String> {
    match &option.kind {
        SessionConfigKind::Select(select) => match &select.options {
            SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .map(|option| option.value.0.to_string())
                .collect(),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|option| option.value.0.to_string())
                .collect(),
            _ => panic!("unexpected select options variant"),
        },
        _ => panic!("expected a select option"),
    }
}

#[test]
fn emits_mode_model_effort_zdr_in_order() {
    let meta = meta();
    let options = session_config_options(&meta, &[], caps());
    let ids: Vec<&str> = options.iter().map(|option| option.id.0.as_ref()).collect();
    assert_eq!(ids, ["mode", "model", "effort", "zdr"]);

    assert_eq!(
        find_option(&options, "mode").category,
        Some(SessionConfigOptionCategory::Mode)
    );
    assert_eq!(
        find_option(&options, "model").category,
        Some(SessionConfigOptionCategory::Model)
    );
    assert_eq!(
        find_option(&options, "effort").category,
        Some(SessionConfigOptionCategory::ThoughtLevel)
    );
    assert_eq!(find_option(&options, "zdr").category, None);
}

#[test]
fn mode_select_lists_all_modes_with_current() {
    let meta = meta();
    let options = session_config_options(&meta, &[], caps());
    let mode = find_option(&options, "mode");
    match &mode.kind {
        SessionConfigKind::Select(select) => {
            assert_eq!(select.current_value.0.as_ref(), "build");
            assert_eq!(select_values(mode), ["build", "auto", "plan", "bypass"]);
        }
        _ => panic!("mode must be a select"),
    }
}

#[test]
fn model_select_groups_by_provider_in_first_seen_order() {
    let meta = meta();
    let models = vec![
        model("bubna", "gemini-3.8-flash", Some(262_144), None),
        model("ollama", "qwen3.5", None, None),
        // A second bubna entry must join the first group, not start a new one.
        model("bubna", "gemini-3.8-pro", Some(1_048_576), None),
    ];
    let options = session_config_options(&meta, &models, caps());
    let model_option = find_option(&options, "model");
    match &model_option.kind {
        SessionConfigKind::Select(select) => {
            assert_eq!(select.current_value.0.as_ref(), "bubna/gemini-3.8-flash");
            match &select.options {
                SessionConfigSelectOptions::Grouped(groups) => {
                    let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
                    assert_eq!(names, ["bubna", "ollama"]);
                    assert_eq!(groups[0].options.len(), 2);
                    assert_eq!(groups[1].options.len(), 1);
                    assert_eq!(
                        groups[0].options[0].value.0.as_ref(),
                        "bubna/gemini-3.8-flash"
                    );
                    assert_eq!(
                        groups[0].options[0].description.as_deref(),
                        Some("262k ctx")
                    );
                    assert_eq!(groups[1].options[0].description, None);
                }
                _ => panic!("model select must be grouped"),
            }
        }
        _ => panic!("model must be a select"),
    }
}

#[test]
fn empty_catalog_keeps_current_route_selectable() {
    let meta = meta();
    let options = session_config_options(&meta, &[], caps());
    let model_option = find_option(&options, "model");
    match &model_option.kind {
        SessionConfigKind::Select(select) => {
            assert_eq!(select.current_value.0.as_ref(), "bubna/gemini-3.8-flash");
            assert_eq!(select_values(model_option), ["bubna/gemini-3.8-flash"]);
        }
        _ => panic!("model must be a select"),
    }
}

#[test]
fn a_route_missing_from_the_catalog_is_appended() {
    let meta = meta();
    let models = vec![model("ollama", "qwen3.5", None, None)];
    let options = session_config_options(&meta, &models, caps());
    let model_option = find_option(&options, "model");
    let values = select_values(model_option);
    assert!(
        values.contains(&"bubna/gemini-3.8-flash".to_string()),
        "current route must stay selectable: {values:?}"
    );
}

#[test]
fn effort_select_honors_supported_efforts() {
    let mut meta = meta();
    meta.effort = Effort::Low;
    let models = vec![model(
        "bubna",
        "gemini-3.8-flash",
        None,
        Some(vec![Effort::Low, Effort::XHigh]),
    )];
    let options = session_config_options(&meta, &models, caps());
    let effort = find_option(&options, "effort");
    match &effort.kind {
        SessionConfigKind::Select(select) => {
            assert_eq!(select.current_value.0.as_ref(), "low");
            assert_eq!(select_values(effort), ["low", "xhigh"]);
        }
        _ => panic!("effort must be a select"),
    }
}

#[test]
fn effort_select_falls_back_to_all_when_unlisted() {
    let meta = meta();
    // Model not in catalog → every effort is selectable.
    let options = session_config_options(&meta, &[], caps());
    let effort = find_option(&options, "effort");
    assert_eq!(
        select_values(effort),
        ["low", "medium", "high", "xhigh", "max"]
    );

    // Catalog entry without supported_efforts → same.
    let models = vec![model("bubna", "gemini-3.8-flash", None, None)];
    let options = session_config_options(&meta, &models, caps());
    let effort = find_option(&options, "effort");
    assert_eq!(
        select_values(effort),
        ["low", "medium", "high", "xhigh", "max"]
    );
}

#[test]
fn a_current_effort_outside_supported_efforts_is_appended_sorted() {
    let meta = meta(); // effort = high
    let models = vec![model(
        "bubna",
        "gemini-3.8-flash",
        None,
        Some(vec![Effort::Low]),
    )];
    let options = session_config_options(&meta, &models, caps());
    let effort = find_option(&options, "effort");
    assert_eq!(select_values(effort), ["low", "high"]);
}

#[test]
fn zdr_is_a_boolean_when_supported() {
    let meta = meta();
    let options = session_config_options(&meta, &[], caps());
    match &find_option(&options, "zdr").kind {
        SessionConfigKind::Boolean(boolean) => assert!(!boolean.current_value),
        _ => panic!("zdr must be a boolean option"),
    }
}

#[test]
fn zdr_degrades_to_a_select_without_boolean_support() {
    let meta = meta();
    let caps = ConfigOptionCaps {
        select: true,
        boolean: false,
    };
    let options = session_config_options(&meta, &[], caps);
    let zdr = find_option(&options, "zdr");
    match &zdr.kind {
        SessionConfigKind::Select(select) => {
            assert_eq!(select.current_value.0.as_ref(), "false");
            assert_eq!(select_values(zdr), ["false", "true"]);
        }
        _ => panic!("zdr must degrade to a select"),
    }
}

#[test]
fn apply_config_value_maps_each_option() {
    let change = apply_config_value("mode", &SessionConfigOptionValue::value_id("plan"))
        .expect("mode value");
    assert_eq!(change, ConfigChange::Mode(Mode::Plan));
    assert!(matches!(change.to_request(), Request::SetMode(Mode::Plan)));

    let change = apply_config_value(
        "model",
        &SessionConfigOptionValue::value_id("ollama/qwen3.5"),
    )
    .expect("model value");
    assert_eq!(
        change,
        ConfigChange::Model(ModelRoute::new("ollama", "qwen3.5"))
    );
    assert!(matches!(change.to_request(), Request::SetModel(_)));

    let change = apply_config_value("effort", &SessionConfigOptionValue::value_id("xhigh"))
        .expect("effort value");
    assert_eq!(change, ConfigChange::Effort(Effort::XHigh));
    assert!(matches!(
        change.to_request(),
        Request::SetEffort(Effort::XHigh)
    ));
}

#[test]
fn apply_config_value_accepts_boolean_and_value_id_for_zdr() {
    let change =
        apply_config_value("zdr", &SessionConfigOptionValue::boolean(true)).expect("zdr boolean");
    assert_eq!(change, ConfigChange::Zdr(true));
    assert!(matches!(change.to_request(), Request::SetZdr(true)));

    let change = apply_config_value("zdr", &SessionConfigOptionValue::value_id("false"))
        .expect("zdr value id");
    assert_eq!(change, ConfigChange::Zdr(false));
}

#[test]
fn apply_config_value_rejects_unknown_ids_and_bad_values() {
    assert!(apply_config_value("nonsense", &SessionConfigOptionValue::value_id("x")).is_err());
    // A select value that is not a mode.
    assert!(apply_config_value("mode", &SessionConfigOptionValue::value_id("nope")).is_err());
    // A boolean where a select value id belongs.
    assert!(apply_config_value("mode", &SessionConfigOptionValue::boolean(true)).is_err());
    // A route without provider/model split.
    assert!(apply_config_value("model", &SessionConfigOptionValue::value_id("noslash")).is_err());
    // A non-boolean value for zdr.
    assert!(apply_config_value("zdr", &SessionConfigOptionValue::value_id("maybe")).is_err());
}

#[test]
fn apply_to_meta_updates_the_matching_field() {
    let mut meta = meta();
    ConfigChange::Mode(Mode::Plan).apply_to_meta(&mut meta);
    assert_eq!(meta.mode, Mode::Plan);
    ConfigChange::Model(ModelRoute::new("ollama", "qwen3.5")).apply_to_meta(&mut meta);
    assert_eq!(meta.model, ModelRoute::new("ollama", "qwen3.5"));
    ConfigChange::Effort(Effort::Max).apply_to_meta(&mut meta);
    assert_eq!(meta.effort, Effort::Max);
    ConfigChange::Zdr(true).apply_to_meta(&mut meta);
    assert!(meta.zdr);
}

#[test]
fn apply_event_updates_meta_and_models() {
    let mut state = SessionConfigState {
        meta: meta(),
        models: Vec::new(),
    };
    let mut new_meta = meta();
    new_meta.mode = Mode::Bypass;
    assert!(apply_event(&mut state, &Event::SessionUpdated(new_meta)));
    assert_eq!(state.meta.mode, Mode::Bypass);

    assert!(apply_event(
        &mut state,
        &Event::ModelsUpdated {
            models: vec![model("ollama", "qwen3.5", None, None)],
        }
    ));
    assert_eq!(state.models.len(), 1);

    assert!(!apply_event(&mut state, &Event::DaemonShuttingDown));
}
