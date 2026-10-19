//! Visual gallery: renders the snapshot fixtures, the model palette and a few
//! prompt states into one HTML page per state and size (every cell with its
//! fg/bg/bold/italic/underline), plus an `index.html`.
//!
//! ```text
//! PACODE_GALLERY_DIR=/tmp/gallery cargo test -p pacode-tui --test gallery -- --ignored
//! ```
//!
//! Public API only, and the builders live in `tests/common`: the pair can be
//! dropped into an older checkout to render the "before" shots.

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use common::*;
use pacode_tui::state::{AppState, Focus, Overlay, PanelTarget};
use pacode_types::ids::AgentId;
use pacode_types::state::{AgentStatus, ProgressSource, TaskProgress, TaskStatus};
use pacode_types::transcript::{DiffStat, ToolStatus};
use pacode_types::{TranscriptItem, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

const SIZES: [(u16, u16); 4] = [(60, 20), (80, 24), (120, 34), (200, 60)];

/// One gallery entry: a name and how to build its state at a size.
struct Shot {
    name: &'static str,
    build: fn(u16, u16) -> AppState,
}

#[test]
#[ignore = "writes HTML for eyes, not assertions; set PACODE_GALLERY_DIR"]
fn gallery() {
    let Some(dir) = std::env::var_os("PACODE_GALLERY_DIR").map(PathBuf::from) else {
        eprintln!("PACODE_GALLERY_DIR is not set; nothing written");
        return;
    };
    std::fs::create_dir_all(&dir).expect("gallery dir");

    let shots = [
        Shot {
            name: "01_base",
            build: base,
        },
        Shot {
            name: "02_select_agent",
            build: select_agent,
        },
        Shot {
            name: "03_panel",
            build: panel,
        },
        Shot {
            name: "05_background_menu",
            build: background_menu,
        },
        Shot {
            name: "06_idle",
            build: idle,
        },
        Shot {
            name: "07_model_palette",
            build: model_palette,
        },
        Shot {
            name: "08_input_multiline",
            build: input_multiline,
        },
        Shot {
            name: "09_input_bash",
            build: input_bash,
        },
        Shot {
            name: "10_input_slash",
            build: input_slash,
        },
        Shot {
            name: "11_bang_result",
            build: bang_result,
        },
    ];

    let mut index = Vec::new();
    for shot in &shots {
        for (w, h) in SIZES {
            let mut state = (shot.build)(w, h);
            force_color(&mut state, "truecolor");
            let file = format!("{}_{w}x{h}.html", shot.name);
            write_shot(
                &dir,
                &file,
                &format!("{} {w}x{h}", shot.name),
                &mut state,
                w,
                h,
            );
            index.push(file);
        }
    }
    // The colour fallbacks, once each: the base screen and the palette (its
    // badges and selection bar are where the fallbacks show most).
    for mode in ["ansi", "mono"] {
        let fallbacks = [
            Shot {
                name: "01_base",
                build: base,
            },
            Shot {
                name: "07_model_palette",
                build: model_palette,
            },
        ];
        for Shot { name, build } in fallbacks {
            let mut state = build(120, 34);
            force_color(&mut state, mode);
            let file = format!("{name}_{mode}_120x34.html");
            write_shot(
                &dir,
                &file,
                &format!("{name} {mode} 120x34"),
                &mut state,
                120,
                34,
            );
            index.push(file);
        }
    }

    let mut html = page_head("pacode gallery");
    html.push_str("<ul>");
    for file in &index {
        let _ = write!(html, "<li><a href=\"{file}\">{file}</a></li>");
    }
    html.push_str("</ul></body></html>");
    std::fs::write(dir.join("index.html"), html).expect("index");
}

fn force_color(state: &mut AppState, mode: &str) {
    state.config.ui.color = mode.to_string();
    state.apply_runtime_config("ui.color");
}

fn write_shot(dir: &Path, file: &str, title: &str, state: &mut AppState, w: u16, h: u16) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal
        .draw(|f| {
            let prepared = pacode_tui::ui::prepare_frame(f.area(), state, Instant::now());
            pacode_tui::ui::draw(f, state, &prepared);
        })
        .expect("draw");
    let mut html = page_head(title);
    let _ = write!(html, "<h1>{}</h1>", escape(title));
    html.push_str(&buffer_html(terminal.backend().buffer()));
    html.push_str("</body></html>");
    std::fs::write(dir.join(file), html).expect("shot");
}

// ---- states ---------------------------------------------------------------

fn base(w: u16, h: u16) -> AppState {
    create_state(w, h, make_base_snapshot())
}

fn select_agent(w: u16, h: u16) -> AppState {
    let mut state = base(w, h);
    state.focus = Focus::SelectAgent { index: 3 };
    settle_focus(&mut state);
    state
}

fn panel(w: u16, h: u16) -> AppState {
    let mut state = base(w, h);
    state.config.ui.agent_view = pacode_types::config::AgentView::Panel;
    let target = PanelTarget::Agent(AgentId::new("agt_3"));
    state.focus = Focus::Panel {
        target: target.clone(),
        follow: false,
        follow_paused: false,
    };
    state.panel.target = Some(target);
    settle_focus(&mut state);
    let item = |seq, kind| TranscriptItem {
        seq,
        agent: AgentId::new("agt_3"),
        ts_ms: seq * 10,
        kind,
    };
    state.panel.agent_transcript.reset(
        vec![
            item(
                101,
                TranscriptKind::User {
                    text: "Update rail.rs with new layout".into(),
                },
            ),
            item(
                102,
                TranscriptKind::Assistant {
                    text: "Here is the diff for rail rendering.".into(),
                    complete: true,
                },
            ),
            item(
                103,
                TranscriptKind::ToolCall {
                    call_id: "call_diff".into(),
                    name: "edit_file".into(),
                    title: "crates/pacode-tui/src/ui/rail.rs".into(),
                    intent: None,
                    status: ToolStatus::Ok,
                    preview: "--- a/rail.rs\n+++ b/rail.rs\n@@ -10,3 +10,5 @@\n+use ratatui::Frame;\n-old_code();\n+new_code();\n".into(),
                    diff: Some(DiffStat {
                        added: 64,
                        removed: 12,
                    }),
                    duration_ms: Some(85),
                    task: None,
                },
            ),
        ],
        false,
    );
    state
}

fn background_menu(w: u16, h: u16) -> AppState {
    let mut snapshot = make_base_snapshot();
    let now = pacode_types::time::now_ms();
    let progress = TaskProgress {
        current: Some(214),
        total: Some(380),
        percent: None,
        message: None,
        source: ProgressSource::Reported,
        updated_at_ms: 1500,
    };
    snapshot.tasks = vec![
        make_task(
            "tsk_1",
            "cargo test --all",
            TaskStatus::Running,
            Some(progress),
            now - 214_000,
            None,
            0,
        ),
        make_task(
            "tsk_2",
            "cargo build --release",
            TaskStatus::Completed,
            None,
            1000,
            Some(183_000),
            0,
        ),
        make_task(
            "tsk_3",
            "npm run lint",
            TaskStatus::Failed,
            None,
            1100,
            Some(1200),
            3,
        ),
    ];
    let mut state = create_state(w, h, snapshot);
    state.focus = Focus::Overlay(Overlay::Menu {
        stack: pacode_tui::ui::menu::MenuStack::root(pacode_tui::ui::menu::MenuId::Background),
        ui: pacode_tui::ui::menu::MenuUiState::default(),
    });
    settle_focus(&mut state);
    state
}

fn idle(w: u16, h: u16) -> AppState {
    let mut snapshot = make_base_snapshot();
    let now = pacode_types::time::now_ms();
    snapshot.turn_active = false;
    for agent in &mut snapshot.agents {
        agent.status = AgentStatus::Finished;
        if agent.id.is_main() {
            agent.finished_at_ms = Some(now);
        }
    }
    snapshot.tasks.clear();
    let mut state = create_state(w, h, snapshot);
    state.turn_active = false;
    state.rail.show_session_stats = true;
    state
}

/// The model screen, opened the way a user opens it: `/model`.
fn model_palette(w: u16, h: u16) -> AppState {
    let mut state = base(w, h);
    let models = serde_json::json!([
        {"route": {"provider": "anthropic", "model": "claude-sonnet-4-5"},
         "display_name": "Claude Sonnet 4.5", "context_window": 200000,
         "supports_reasoning": true,
         "pricing": {"input_per_m": 3.0, "output_per_m": 15.0}},
        {"route": {"provider": "anthropic", "model": "claude-opus-4-1"},
         "display_name": "Claude Opus 4.1", "context_window": 200000,
         "supports_reasoning": true,
         "pricing": {"input_per_m": 15.0, "output_per_m": 75.0}},
        {"route": {"provider": "openrouter", "model": "google/gemini-2.5-pro"},
         "display_name": "Gemini 2.5 Pro", "context_window": 1000000,
         "supports_reasoning": true}
    ]);
    state.models = serde_json::from_value(models).expect("models");
    // Built from JSON like the models, so the file compiles against any
    // checkout whose wire types parse it.
    let auth = serde_json::json!([
        {"id": "anthropic", "display_name": "Anthropic", "auth_kind": "oauth",
         "detail": "", "recommended": true, "state": {"type": "configured"},
         "accounts": ["work"], "active": "work"},
        {"id": "openrouter", "display_name": "OpenRouter", "auth_kind": "api_key",
         "detail": "", "recommended": false, "state": {"type": "configured"}},
        {"id": "google", "display_name": "Google", "auth_kind": "oauth",
         "detail": "", "recommended": false,
         "state": {"type": "needs_attention", "reason": "token expired"}},
        {"id": "openai", "display_name": "OpenAI", "auth_kind": "oauth",
         "detail": "", "recommended": false, "state": {"type": "not_configured"}}
    ]);
    state.auth_providers = serde_json::from_value(auth).expect("auth");
    let _ = pacode_tui::commands::execute(&mut state, "/model");
    settle_focus(&mut state);
    state
}

fn input_multiline(w: u16, h: u16) -> AppState {
    let mut state = base(w, h);
    state
        .input
        .insert_str("make me a weather bot\nwith a CLI first, then a Telegram front end");
    state
}

fn input_bash(w: u16, h: u16) -> AppState {
    let mut state = base(w, h);
    state.input.insert_str(
        "!cargo test -p pacode-tui --test snapshot_tests -- --nocapture 2>&1 | grep -E \"FAILED|panicked\"",
    );
    state
}

fn input_slash(w: u16, h: u16) -> AppState {
    let mut state = base(w, h);
    state.input.insert_str("/effort");
    state
}

/// A finished `!cmd`, a failed tool call and an error notice under the base
/// transcript.
fn bang_result(w: u16, h: u16) -> AppState {
    let mut snapshot = make_base_snapshot();
    let item = |seq, kind| TranscriptItem {
        seq,
        agent: AgentId::main(),
        ts_ms: 2000 + seq,
        kind,
    };
    snapshot.transcript.push(item(
        4,
        TranscriptKind::ToolCall {
            call_id: "call_2".into(),
            name: "edit".into(),
            title: "edit src/main.rs".into(),
            intent: None,
            status: ToolStatus::Error,
            preview: "old_string not found".into(),
            diff: None,
            duration_ms: Some(30),
            task: None,
        },
    ));
    snapshot.transcript.push(item(
        5,
        TranscriptKind::BashCommand {
            command: "python3 --version".into(),
            output: "Python 3.14.0".into(),
            exit_code: Some(0),
            truncated: false,
        },
    ));
    snapshot.transcript.push(item(
        6,
        TranscriptKind::Notice {
            level: pacode_types::ToastLevel::Error,
            text: "turn failed: request failed (400): model foo is not supported".into(),
        },
    ));
    create_state(w, h, snapshot)
}

// ---- HTML -----------------------------------------------------------------

fn page_head(title: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>\
         body{{background:#111;color:#c9c7c0;font-family:sans-serif;margin:16px}}\
         a{{color:#6ea9bd}}\
         pre{{font-family:'JetBrains Mono','DejaVu Sans Mono',monospace;font-size:13px;\
         line-height:1.2;background:#161616;padding:8px;display:inline-block}}\
         </style></head><body>",
        escape(title)
    )
}

/// The buffer as one `<pre>`, consecutive cells of one style merged into a span.
fn buffer_html(buffer: &Buffer) -> String {
    let mut out = String::from("<pre>");
    for y in 0..buffer.area.height {
        let mut run_style = String::new();
        let mut run_text = String::new();
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let style = cell_css(cell.fg, cell.bg, cell.modifier);
            if style != run_style && !run_text.is_empty() {
                push_run(&mut out, &run_style, &run_text);
                run_text.clear();
            }
            run_style = style;
            run_text.push_str(cell.symbol());
        }
        push_run(&mut out, &run_style, &run_text);
        out.push('\n');
    }
    out.push_str("</pre>");
    out
}

fn push_run(out: &mut String, style: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = write!(out, "<span style=\"{style}\">{}</span>", escape(text));
}

fn cell_css(fg: Color, bg: Color, modifier: Modifier) -> String {
    let (mut fg, mut bg) = (css_color(fg, "#c9c7c0"), css_color(bg, "transparent"));
    if modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
        if fg == "transparent" {
            fg = "#161616".to_string();
        }
    }
    let mut css = format!("color:{fg};background:{bg};");
    if modifier.contains(Modifier::BOLD) {
        css.push_str("font-weight:bold;");
    }
    if modifier.contains(Modifier::ITALIC) {
        css.push_str("font-style:italic;");
    }
    if modifier.contains(Modifier::UNDERLINED) {
        css.push_str("text-decoration:underline;");
    }
    if modifier.contains(Modifier::DIM) {
        css.push_str("opacity:0.6;");
    }
    css
}

/// xterm's default ANSI palette for the named colours.
fn css_color(color: Color, default: &str) -> String {
    match color {
        Color::Reset => default.to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => "#000000".into(),
        Color::Red => "#cd0000".into(),
        Color::Green => "#00cd00".into(),
        Color::Yellow => "#cdcd00".into(),
        Color::Blue => "#0000ee".into(),
        Color::Magenta => "#cd00cd".into(),
        Color::Cyan => "#00cdcd".into(),
        Color::Gray => "#e5e5e5".into(),
        Color::DarkGray => "#7f7f7f".into(),
        Color::LightRed => "#ff0000".into(),
        Color::LightGreen => "#00ff00".into(),
        Color::LightYellow => "#ffff00".into(),
        Color::LightBlue => "#5c5cff".into(),
        Color::LightMagenta => "#ff00ff".into(),
        Color::LightCyan => "#00ffff".into(),
        Color::White => "#ffffff".into(),
        Color::Indexed(i) => format!("var(--ansi-{i}, #888)"),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
