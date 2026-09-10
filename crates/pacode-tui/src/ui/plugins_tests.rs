use pacode_render::RenderOptions;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

fn installed(name: &str) -> PluginInfo {
    PluginInfo {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        kind: "lua".to_string(),
        tools: vec!["do_thing".to_string()],
        commands: Vec::new(),
        error: None,
        loaded: true,
        ..PluginInfo::default()
    }
}

fn marketplace(name: &str) -> PluginInfo {
    PluginInfo {
        name: name.to_string(),
        version: "2.1.0".to_string(),
        description: "drives a real browser".to_string(),
        author: "someone".to_string(),
        source: "owner/repo".to_string(),
        installed_at_ms: 1_700_000_000_000,
        mcp_servers: vec!["playwright".to_string()],
        skill_dirs: 2,
        unsupported: vec!["hooks".to_string()],
        ..PluginInfo::default()
    }
}

fn offered(name: &str, installed_version: Option<&str>, listed: &str) -> MarketplacePluginInfo {
    MarketplacePluginInfo {
        name: name.to_string(),
        description: "does something useful".to_string(),
        version: listed.to_string(),
        author: "someone".to_string(),
        category: "mcp".to_string(),
        marketplace: "owner/repo".to_string(),
        installed: installed_version.is_some(),
        installed_version: installed_version.map(str::to_string),
        update_available: installed_version.is_some_and(|v| v != listed),
    }
}

fn render(view: &PluginsView<'_>) -> String {
    let opts = RenderOptions::new(72, false);
    let backend = TestBackend::new(72, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| draw(f, f.area(), view, &opts))
        .expect("draw");
    format!("{}", terminal.backend())
}

#[test]
fn the_installed_tab_lists_loaded_plugins() {
    let plugins = [installed("greeter"), installed("linter")];
    let view = PluginsView {
        index: 0,
        plugins: &plugins,
        tab: PluginsTab::Installed,
        market: &[],
        query: "",
        loading: false,
        stale: false,
        source: "owner/repo",
    };
    let text = render(&view);
    assert!(text.contains("Installed"), "{text}");
    assert!(text.contains("greeter"), "{text}");
    assert!(text.contains("linter"), "{text}");
    assert!(text.contains("x uninstall"), "{text}");
}

#[test]
fn the_installed_tab_shows_marketplace_plugins() {
    let plugins = [marketplace("playwright")];
    let view = PluginsView {
        index: 0,
        plugins: &plugins,
        tab: PluginsTab::Installed,
        market: &[],
        query: "",
        loading: false,
        stale: false,
        source: "owner/repo",
    };
    let text = render(&view);
    assert!(text.contains("playwright"), "{text}");
    assert!(text.contains("drives a real browser"), "{text}");
    assert!(text.contains("owner/repo"), "{text}");
    assert!(text.contains("mcp: playwright"), "{text}");
    assert!(
        text.contains("hooks are declared but not applied"),
        "unsupported components must be visible: {text}"
    );
}

#[test]
fn an_empty_installed_tab_says_so() {
    let view = PluginsView {
        index: 0,
        plugins: &[],
        tab: PluginsTab::Installed,
        market: &[],
        query: "",
        loading: false,
        stale: false,
        source: "",
    };
    let text = render(&view);
    assert!(text.contains("No plugins installed"), "{text}");
    assert!(text.contains("tab Discover"), "{text}");
}

#[test]
fn discover_without_a_marketplace_says_how_to_add_one() {
    let view = PluginsView {
        index: 0,
        plugins: &[],
        tab: PluginsTab::Discover,
        market: &[],
        query: "",
        loading: false,
        stale: false,
        source: "",
    };
    let text = render(&view);
    assert!(text.contains("No marketplace configured"), "{text}");
    assert!(text.contains("/plugins owner/repo"), "{text}");
}

#[test]
fn discover_shows_the_search_box_installed_marks_and_updates() {
    let market = [
        offered("context7", None, "1.2.0"),
        offered("skill-creator", Some("0.9.0"), "1.0.0"),
    ];
    let view = PluginsView {
        index: 0,
        plugins: &[],
        tab: PluginsTab::Discover,
        market: &market,
        query: "cre",
        loading: false,
        stale: false,
        source: "owner/repo",
    };
    let text = render(&view);
    assert!(text.contains("> cre"), "{text}");
    assert!(text.contains("context7"), "{text}");
    assert!(
        text.contains("update"),
        "an available update must be visible: {text}"
    );
    assert!(text.contains("i install"), "{text}");
}

#[test]
fn a_stale_listing_says_it_is_an_offline_copy() {
    let market = [offered("context7", None, "1.2.0")];
    let view = PluginsView {
        index: 0,
        plugins: &[],
        tab: PluginsTab::Discover,
        market: &market,
        query: "",
        loading: false,
        stale: true,
        source: "owner/repo",
    };
    assert!(render(&view).contains("offline copy"));

    let loading = PluginsView {
        stale: false,
        loading: true,
        ..PluginsView {
            index: 0,
            plugins: &[],
            tab: PluginsTab::Discover,
            market: &[],
            query: "",
            loading: true,
            stale: false,
            source: "owner/repo",
        }
    };
    assert!(render(&loading).contains("loading"));
}

#[test]
fn the_window_keeps_the_selected_row_on_screen() {
    assert_eq!(window(0, 0, 5), (0, 0));
    assert_eq!(window(0, 10, 0), (0, 0));
    assert_eq!(window(0, 10, 4), (0, 0));
    assert_eq!(window(3, 10, 4), (0, 3));
    assert_eq!(window(4, 10, 4), (1, 4));
    // An index past the end clamps to the last row rather than panicking.
    assert_eq!(window(99, 10, 4), (6, 9));
}
