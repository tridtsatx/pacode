use pacode_types::{AuthState, Effort, ModelFlags, ModelInfo, ModelRoute, ProviderAuthInfo};

use super::*;
use crate::ui::model_catalog::Flags;

fn model(provider: &str, id: &str, name: &str) -> ModelInfo {
    ModelInfo {
        route: ModelRoute::new(provider, id),
        display_name: name.to_string(),
        context_window: Some(200_000),
        supports_reasoning: true,
        supports_vision: false,
        supported_efforts: None,
        pricing: None,
    }
}

fn auth(id: &str, name: &str, state: AuthState) -> ProviderAuthInfo {
    ProviderAuthInfo {
        id: id.to_string(),
        display_name: name.to_string(),
        auth_kind: "oauth".to_string(),
        detail: String::new(),
        recommended: false,
        state,
        accounts: Vec::new(),
        active: None,
    }
}

struct Fixture {
    models: Vec<ModelInfo>,
    flags: Flags,
    auth: Vec<ProviderAuthInfo>,
    recent: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            models: vec![
                model("claude", "sonnet", "Claude Sonnet 5.5"),
                model("claude", "opus", "Claude Opus 5.5"),
                model("bubna", "flash", "Gemini Flash"),
                model("stale", "old", "Stale Model"),
            ],
            flags: Flags::new(),
            auth: vec![
                auth("claude", "Claude", AuthState::Configured),
                auth(
                    "stale",
                    "Stale",
                    AuthState::NeedsAttention {
                        reason: "token expired".to_string(),
                    },
                ),
                auth("openrouter", "OpenRouter", AuthState::NotConfigured),
            ],
            recent: vec!["claude/opus".to_string()],
        }
    }

    fn src(&self) -> Sources<'_> {
        Sources {
            models: &self.models,
            flags: &self.flags,
            auth: &self.auth,
            recent: &self.recent,
        }
    }
}

/// One line per row: `#` header, `-` gap, `m` model (`r` recent).
fn shape(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            Row::Gap => "-".to_string(),
            Row::Header { title, .. } => format!("# {title}"),
            Row::Model { model, recent, .. } => {
                format!("{} {}", if *recent { "r" } else { "m" }, model.route)
            }
        })
        .collect()
}

#[test]
fn groups_run_recent_then_signed_in_then_needing_a_sign_in() {
    let fx = Fixture::new();
    let rows = ModelPalette::default().rows(&fx.src());
    assert_eq!(
        shape(&rows),
        vec![
            "# RECENT",
            "r claude/opus",
            "-",
            "# BUBNA",
            "m bubna/flash",
            "-",
            "# CLAUDE",
            "m claude/opus",
            "m claude/sonnet",
            "-",
            "# STALE",
            "m stale/old",
            "-",
            "# OPENROUTER",
        ]
    );
    // Every header with an auth record is selectable (sign in, add an
    // account, switch, sign out); `RECENT` and config-only providers are not.
    let header = |title: &str| {
        rows.iter()
            .find(|row| matches!(row, Row::Header { title: t, .. } if t == title))
            .cloned()
    };
    assert!(header("CLAUDE").is_some_and(|r| r.selectable()));
    assert!(header("STALE").is_some_and(|r| r.selectable()));
    assert!(header("OPENROUTER").is_some_and(|r| r.selectable()));
    assert!(!header("BUBNA").is_some_and(|r| r.selectable()));
    assert!(!header("RECENT").is_some_and(|r| r.selectable()));
    assert!(matches!(
        header("OPENROUTER"),
        Some(Row::Header {
            kind: Some("OAuth"),
            ..
        })
    ));
    assert!(matches!(
        header("CLAUDE"),
        Some(Row::Header {
            status: GroupStatus::SignedIn,
            ..
        })
    ));
    assert!(matches!(
        header("BUBNA"),
        Some(Row::Header {
            status: GroupStatus::Plain,
            ..
        })
    ));
}

#[test]
fn fuzzy_search_ranks_inside_groups_and_filters_recent() {
    let mut fx = Fixture::new();
    fx.recent = vec!["claude/opus".to_string(), "claude/sonnet".to_string()];
    let mut palette = ModelPalette::default();
    for c in "son".chars() {
        palette.push_char(c);
    }
    let rows = palette.rows(&fx.src());
    let shape = shape(&rows);
    // RECENT stays while searching, holding only the recents that match.
    assert_eq!(shape[..2], ["# RECENT", "r claude/sonnet"], "{shape:?}");
    assert_eq!(shape[3], "# CLAUDE");
    assert_eq!(
        shape[4], "m claude/sonnet",
        "the best match leads: {shape:?}"
    );
    // The provider name is searched too: "router" finds the bare provider.
    let mut palette = ModelPalette::default();
    palette.push_str("router");
    assert_eq!(shape_of(&palette, &fx), vec!["# OPENROUTER"]);
}

fn shape_of(palette: &ModelPalette, fx: &Fixture) -> Vec<String> {
    shape(&palette.rows(&fx.src()))
}

#[test]
fn recent_is_capped_and_skips_routes_the_catalog_lost() {
    let mut fx = Fixture::new();
    fx.models = (0..8)
        .map(|i| model("p", &format!("m{i}"), &format!("M{i}")))
        .collect();
    fx.recent = (0..8).rev().map(|i| format!("p/m{i}")).collect();
    fx.recent.insert(1, "gone/model".to_string());
    let rows = ModelPalette::default().rows(&fx.src());
    let recent: Vec<String> = shape(&rows)
        .into_iter()
        .filter(|line| line.starts_with("r "))
        .collect();
    assert_eq!(
        recent,
        vec!["r p/m7", "r p/m6", "r p/m5", "r p/m4", "r p/m3"],
        "most recent first, at most {RECENT_CAP}, unknown routes skipped"
    );
}

#[test]
fn filtering_keeps_the_selection_in_range() {
    let fx = Fixture::new();
    let mut palette = ModelPalette {
        cursor: 50,
        ..ModelPalette::default()
    };
    let rows = palette.rows(&fx.src());
    let index = palette.selected_index(&rows).expect("a selection");
    assert!(
        rows[index].selectable(),
        "clamped onto the last selectable row"
    );

    palette.push_str("flash");
    let rows = palette.rows(&fx.src());
    assert_eq!(palette.cursor, 0, "a new query selects the best match");
    assert!(matches!(
        palette.selected(&rows),
        Some(Row::Model { model, .. }) if model.route.to_string() == "bubna/flash"
    ));

    palette.push_str("zzzz");
    let rows = palette.rows(&fx.src());
    assert_eq!(
        palette.selected_index(&rows),
        None,
        "nothing left to select"
    );
    palette.move_by(1, &rows);
    assert_eq!(palette.selected_index(&rows), None);
}

#[test]
fn moving_skips_gaps_and_signed_in_headers_and_clamps() {
    let fx = Fixture::new();
    let mut palette = ModelPalette::default();
    let rows = palette.rows(&fx.src());
    palette.move_by(-5, &rows);
    assert_eq!(
        palette.selected_index(&rows),
        Some(1),
        "first selectable row"
    );
    palette.move_by(1, &rows);
    assert_eq!(
        palette.selected_index(&rows),
        Some(4),
        "over the gap and BUBNA"
    );
    palette.move_by(100, &rows);
    assert_eq!(palette.selected_index(&rows), Some(rows.len() - 1));
}

/// B04: with ZDR on only marked routes are offered — but signing in has
/// nothing to do with ZDR, so every provider with an auth record keeps its
/// header (that is all `/login` under ZDR has to show).
#[test]
fn zdr_only_filters_models_but_keeps_sign_in_headers() {
    let mut fx = Fixture::new();
    fx.flags.insert(
        "claude/sonnet".to_string(),
        ModelFlags {
            zdr_capable: true,
            favorite: false,
        },
    );
    let palette = ModelPalette::new(Effort::Medium, true, false, None);
    let shape = shape_of(&palette, &fx);
    assert_eq!(
        shape,
        vec![
            "# CLAUDE",
            "m claude/sonnet",
            "-",
            "# OPENROUTER",
            "-",
            "# STALE"
        ]
    );
}

#[test]
fn the_kind_filter_narrows_by_capability() {
    let mut fx = Fixture::new();
    fx.models[0].supports_vision = true;
    let mut palette = ModelPalette::default();
    palette.cycle_kind(); // reasoning: all fixtures reason
    assert!(shape_of(&palette, &fx).contains(&"m claude/opus".to_string()));
    palette.cycle_kind(); // vision
    let shape = shape_of(&palette, &fx);
    assert!(shape.contains(&"m claude/sonnet".to_string()));
    assert!(!shape.contains(&"m claude/opus".to_string()), "{shape:?}");
}

/// B13: the effort shown and sent is snapped to whatever model is
/// highlighted, however the highlight got there.
#[test]
fn the_effort_follows_the_highlighted_models_ladder() {
    let mut fx = Fixture::new();
    fx.models[0].supported_efforts = Some(vec![Effort::Low, Effort::High]);
    let palette = ModelPalette::new(Effort::Max, false, false, None);
    assert_eq!(palette.effort_for(&fx.models[0]), Some(Effort::High));
    assert_eq!(palette.effort_for(&fx.models[1]), Some(Effort::Max));
}

/// Tab steps only over the levels the highlighted model accepts, and only
/// moves the palette's own effort.
#[test]
fn effort_cycling_walks_the_accepted_levels() {
    let mut m = model("p", "m", "M");
    m.supported_efforts = Some(vec![Effort::Low, Effort::High, Effort::Max]);
    let mut palette = ModelPalette::new(Effort::Medium, false, false, None);
    assert_eq!(palette.effort_for(&m), Some(Effort::Low));
    palette.cycle_effort(true, &m);
    assert_eq!(palette.effort, Effort::High);
    palette.cycle_effort(true, &m);
    assert_eq!(palette.effort, Effort::Max);
    palette.cycle_effort(true, &m);
    assert_eq!(
        palette.effort,
        Effort::Low,
        "wraps to the first accepted level"
    );
    palette.cycle_effort(false, &m);
    assert_eq!(palette.effort, Effort::Max);
}

#[test]
fn anchors_land_on_the_session_route_or_the_first_sign_in() {
    let mut fx = Fixture::new();
    fx.recent.clear();
    let palette = ModelPalette::new(
        Effort::Medium,
        false,
        false,
        Some(Anchor::Route(ModelRoute::new("claude", "sonnet"))),
    );
    // The anchor waits for rows that are still loading.
    let empty = Sources {
        models: &[],
        flags: &fx.flags,
        auth: &[],
        recent: &[],
    };
    assert_eq!(palette.selected_index(&palette.rows(&empty)), None);
    let rows = palette.rows(&fx.src());
    assert!(matches!(
        palette.selected(&rows),
        Some(Row::Model { model, .. }) if model.route.to_string() == "claude/sonnet"
    ));

    let palette = ModelPalette::new(Effort::Medium, false, false, Some(Anchor::FirstSignIn));
    let rows = palette.rows(&fx.src());
    assert!(matches!(
        palette.selected(&rows),
        Some(Row::Header { title, .. }) if title == "STALE"
    ));

    // The first move drops the anchor and continues from where it was.
    let mut palette = palette;
    palette.move_by(1, &rows);
    assert!(matches!(
        palette.selected(&rows),
        Some(Row::Model { model, .. }) if model.route.to_string() == "stale/old"
    ));
}

#[test]
fn rows_in_an_unsigned_group_are_marked_for_sign_in() {
    let fx = Fixture::new();
    let rows = ModelPalette::default().rows(&fx.src());
    let signed = |route: &str| {
        rows.iter().find_map(|row| {
            if let Row::Model {
                model,
                signed_in,
                recent: false,
            } = row
                && model.route.to_string() == route
            {
                Some(*signed_in)
            } else {
                None
            }
        })
    };
    assert_eq!(signed("claude/opus"), Some(true));
    assert_eq!(signed("bubna/flash"), Some(true), "no auth record: usable");
    assert_eq!(signed("stale/old"), Some(false));
}

/// A provider that is signed in but lists nothing (an empty or failed
/// catalog) is still a group: its account and logout chords must reach it.
#[test]
fn a_signed_in_provider_with_an_empty_catalog_keeps_its_header() {
    let mut fx = Fixture::new();
    fx.auth.push(auth("devin", "Devin", AuthState::Configured));
    let rows = ModelPalette::default().rows(&fx.src());
    let devin = rows
        .iter()
        .find(|row| matches!(row, Row::Header { title, .. } if title == "DEVIN"))
        .expect("DEVIN header");
    assert!(devin.selectable());
    assert!(matches!(
        devin,
        Row::Header {
            status: GroupStatus::SignedIn,
            ..
        }
    ));
    // Signed-in before needing a sign-in; with models before without.
    let shape = shape(&rows);
    let at = |line: &str| shape.iter().position(|l| l == line);
    assert!(at("# CLAUDE") < at("# DEVIN"));
    assert!(at("# DEVIN") < at("# STALE"), "{shape:?}");
}

/// The bare headers are searched on what the old login screen searched:
/// name, id, how it signs in and its help line.
#[test]
fn provider_search_matches_kind_and_detail() {
    let mut fx = Fixture::new();
    fx.auth[2].auth_kind = "api_key".to_string();
    fx.auth[2].detail = "one key, hundreds of models".to_string();
    let mut palette = ModelPalette::default();
    palette.push_str("hundreds");
    assert_eq!(shape_of(&palette, &fx), vec!["# OPENROUTER"]);
    palette.query = "api_key".to_string();
    let rows = palette.rows(&fx.src());
    assert!(matches!(
        rows.as_slice(),
        [Row::Header {
            kind: Some("API key"),
            ..
        }]
    ));
}

/// After a sign-in the highlight follows the provider's group as it gains
/// models, instead of staying on a position the new rows shifted.
#[test]
fn a_finished_sign_in_moves_the_selection_to_the_provider() {
    let mut fx = Fixture::new();
    fx.recent.clear();
    let mut palette = ModelPalette::new(Effort::Medium, false, false, Some(Anchor::FirstSignIn));
    palette.follow_provider("openrouter");
    let rows = palette.rows(&fx.src());
    assert!(matches!(
        palette.selected(&rows),
        Some(Row::Header { title, .. }) if title == "OPENROUTER"
    ));
    fx.auth[2].state = AuthState::Configured;
    fx.models.push(model("openrouter", "z-ai/glm", "GLM"));
    let rows = palette.rows(&fx.src());
    assert!(matches!(
        palette.selected(&rows),
        Some(Row::Model { model, .. }) if model.route.to_string() == "openrouter/z-ai/glm"
    ));
}
