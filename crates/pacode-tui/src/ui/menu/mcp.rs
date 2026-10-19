//! The `MCP` level (§5.3): one `Item` per server — status glyph on the left,
//! counts or the failure reason on the right; the detail card carries the
//! transport, the command or URL, the capped error tail and the
//! tools/resources/prompts counters.
//!
//! Actions: `r` restarts the selected server, `enter` toggles enabled (the
//! base owns `enter` — it reaches `on_row`), `l` opens the server's
//! daemon-log tail in the side panel — a panel, not a menu level, so the
//! stack is dropped by pointing `focus` at it.
//!
//! Spec gap: the protocol's [`McpServerInfo`] carries no exit code, restart
//! counter or last-attempt timestamp — the detail card shows what the daemon
//! actually reports (§5.3 lists more than the protocol has today).

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers};
use pacode_types::{McpServerInfo, Request};

use crate::binding::Binding;
use crate::keys::Action;
use crate::state::{AppState, Focus, PanelTarget};

use super::model::{Detail, DetailLine, Mark, MenuRow, RowId};
use super::stack::MenuStack;
use super::view::{EmptyState, MenuAction, MenuId, MenuUiState, MenuView, StatusLine};

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;

/// The declared `r` action's label — `on_action` matches on it.
const ACT_RESTART: &str = "restart";
/// The declared `l` action's label.
const ACT_LOG: &str = "log";

/// Error text kept in a detail card: the last ≤ 4 KiB (§5.3 cap) — the tail
/// is kept because chained errors name their root cause at the end.
const ERROR_CAP_BYTES: usize = 4 * 1024;
/// Of those bytes, at most this many tail lines are shown: the detail panel
/// has no scroll of its own, so a longer error is cut with a marker line
/// pointing at the `l` log rather than stretching the card.
const ERROR_MAX_LINES: usize = 12;

/// Daemon-log bytes scanned from the tail on `l` — the file is append-only
/// and unbounded, so the read window is capped rather than whole.
const LOG_READ_CAP: u64 = 256 * 1024;
/// Matching lines the log panel keeps — the newest end wins; a log view is
/// recent history, not an archive.
const LOG_LINE_CAP: usize = 400;

/// The menu view — rebuilt per frame from `state.mcp_servers` (§8.2 seeds it
/// from the disk cache before the first frame; `lists` owns the badge).
pub fn view(state: &AppState, _ui: &MenuUiState) -> MenuView {
    let mut view = MenuView::new(MenuId::Mcp);
    view.status = match state.lists.badge(crate::cache::ListKind::McpServers) {
        Some(crate::cache::Badge::Loading) => Some(StatusLine::loading()),
        Some(crate::cache::Badge::OfflineCopy) => Some(StatusLine::offline_copy()),
        None => None,
    };
    view.rows = state
        .mcp_servers
        .iter()
        .map(|s| server_row(state, s))
        .collect();
    // No filter: `r`/`l` are bare-char actions — a live filter would eat them
    // as query input (§3.4 gives printable keys to the query while it shows).
    view.empty = if state.lists.mcp.in_flight {
        EmptyState::new("loading…", "the server list is on its way")
    } else {
        EmptyState::new(
            "no MCP servers",
            "add mcp.servers to config, or /import to discover",
        )
    };
    view.with_actions(vec![
        action(KeyCode::Char('r'), ACT_RESTART),
        action(KeyCode::Char('l'), ACT_LOG),
    ])
}

/// `enter` on a server row — toggle enabled (§5.3): re-enable a `disabled`
/// server, disable any other. The refresh keeps the list honest until the
/// daemon pushes its answer.
pub fn on_row(
    state: &mut AppState,
    _stack: &mut MenuStack,
    _ui: &mut MenuUiState,
    row: &RowId,
) -> Vec<Action> {
    let name = row.as_str();
    let Some(server) = state.mcp_servers.iter().find(|s| s.name == name) else {
        return vec![];
    };
    let enabled = server.status == "disabled";
    vec![
        Action::Send(Request::SetMcpServerEnabled {
            server: name.to_string(),
            enabled,
        }),
        Action::Send(Request::ListMcpServers),
    ]
}

/// Declared actions (§3.4): `r` restarts the selected server, `l` swaps the
/// menu for the side panel on the server's daemon-log tail.
pub fn on_action(
    state: &mut AppState,
    _ui: &mut MenuUiState,
    action: &MenuAction,
    row: Option<&RowId>,
) -> Vec<Action> {
    let Some(row) = row else {
        return vec![];
    };
    let name = row.as_str().to_string();
    match action.label {
        ACT_RESTART => vec![
            Action::Send(Request::RestartMcpServer { server: name }),
            Action::Send(Request::ListMcpServers),
        ],
        ACT_LOG => {
            open_log(state, &name);
            vec![]
        }
        _ => vec![],
    }
}

/// The status glyph (§5.3): `●` ready, `✗` failed, `⊘` disabled, `◍`
/// starting; `■` covers `stopped` and anything unrecognised. `pub(crate)` so
/// the panel header can draw the same glyph for the same status.
pub(crate) fn status_mark(status: &str) -> Mark {
    match status {
        "ready" => Mark::Active,
        "failed" => Mark::Fail,
        "disabled" => Mark::Disabled,
        "starting" => Mark::Pending,
        _ => Mark::Stopped,
    }
}

/// One declared action on a bare char — the hint edge and the detail chips
/// share the constructor.
fn action(code: KeyCode, label: &'static str) -> MenuAction {
    MenuAction::new(
        Binding {
            code,
            mods: KeyModifiers::NONE,
        },
        label,
    )
}

fn server_row(state: &AppState, s: &McpServerInfo) -> MenuRow {
    MenuRow::Item {
        id: RowId::new(s.name.clone()),
        mark: status_mark(&s.status),
        label: s.name.clone(),
        right: Some(right_tail(s)),
        detail: server_detail(state, s),
    }
}

/// The right-hand tail: counts while ready, the first error line on failure,
/// the bare status word otherwise — the legacy sub-row compressed into one.
fn right_tail(s: &McpServerInfo) -> String {
    match s.status.as_str() {
        "ready" => format!(
            "{} tools · {} res · {} prompts",
            s.tools, s.resources, s.prompts
        ),
        "failed" => match s.error.as_deref().and_then(|e| e.lines().next()) {
            Some(first) => format!("failed: {first}"),
            None => "failed".to_string(),
        },
        other => other.to_string(),
    }
}

fn server_detail(state: &AppState, s: &McpServerInfo) -> Detail {
    let mut lines = vec![field("status", &s.status)];
    match state.config.mcp.servers.get(&s.name) {
        Some(cfg) if cfg.url.is_some() => {
            lines.push(field("transport", "http"));
            if let Some(url) = &cfg.url {
                lines.push(field("url", url));
            }
        }
        Some(cfg) => {
            lines.push(field("transport", "stdio"));
            let command = format!("{} {}", cfg.command, cfg.args.join(" "));
            lines.push(field("command", command.trim()));
        }
        // Servers outside `mcp.servers` are plugin-provided — the daemon
        // registers them as `<plugin>/<server>` (wiring.rs) and the client
        // never sees their launch command.
        None => match s.name.split_once('/') {
            Some((plugin, _)) => lines.push(field("source", format!("plugin {plugin}"))),
            None => lines.push(field("source", "external")),
        },
    }
    if s.status == "ready" {
        lines.push(field(
            "capabilities",
            format!(
                "{} tools · {} resources · {} prompts",
                s.tools, s.resources, s.prompts
            ),
        ));
    }
    if !s.prompt_names.is_empty() {
        lines.push(field("prompts", s.prompt_names.join(", ")));
    }
    if s.status == "disabled" {
        lines.push(DetailLine::Text("enter re-enables".to_string()));
    }
    if let Some(err) = &s.error {
        lines.extend(error_lines(err));
    }
    Detail {
        title: s.name.clone(),
        lines,
        actions: vec![
            action(KeyCode::Char('r'), ACT_RESTART),
            action(KeyCode::Char('l'), ACT_LOG),
        ],
        search: Vec::new(),
    }
}

fn field(label: &str, value: impl Into<String>) -> DetailLine {
    DetailLine::Field {
        label: label.to_string(),
        value: value.into(),
    }
}

/// The last ≤ [`ERROR_CAP_BYTES`] of `err`, then the last
/// [`ERROR_MAX_LINES`] lines of that — the detail panel has no scroll of its
/// own, so a longer error gets the tail plus a marker naming the cut and the
/// `l` log action that shows the whole thing.
fn error_lines(err: &str) -> Vec<DetailLine> {
    let capped = tail_bytes(err, ERROR_CAP_BYTES);
    let all: Vec<&str> = capped.lines().collect();
    let shown = all.len().min(ERROR_MAX_LINES);
    let mut out = Vec::with_capacity(shown + 1);
    if all.len() > shown || capped.len() < err.len() {
        out.push(DetailLine::Text(format!(
            "... error cut: showing {} of {} bytes (l opens the log)",
            capped.len(),
            err.len(),
        )));
    }
    out.extend(
        all[all.len() - shown..]
            .iter()
            .map(|l| DetailLine::Text((*l).to_string())),
    );
    out
}

/// `&s[len - cap..]` snapped to a char boundary — the error tail keeps whole
/// UTF-8 characters whatever it cuts through.
fn tail_bytes(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut start = s.len() - cap;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// The refresh-on-open hook (§8.2): the status list is live data — ask the
/// daemon for a fresh copy; `lists` turns the in-flight flag into the
/// `loading…` badge. The seed is whatever is already in `state.mcp_servers`.
pub fn on_open(_state: &AppState) -> Vec<Action> {
    vec![Action::Send(Request::ListMcpServers)]
}

/// `l` — the daemon log's tail filtered to this server, shown in the side
/// panel (§5.3: "opens a panel, not a menu"). The daemon is local (unix
/// socket), so its log file is readable here. The read is capped and the
/// panel keeps a snapshot — it does not follow new lines; reopen to refresh.
fn open_log(state: &mut AppState, name: &str) {
    let lines = read_server_log(&state.paths.daemon_log(), name);
    state.panel.task_total_lines = lines.len() as u64;
    state.panel.task_lines = lines;
    // For this target `panel.scroll` is the offset from the tail — 0 shows
    // the newest lines (panel.rs documents the per-target semantics).
    state.panel.scroll = 0;
    let target = PanelTarget::McpServer(name.to_string());
    state.panel.target = Some(target.clone());
    state.focus = Focus::Panel {
        target,
        follow: false,
        follow_paused: false,
    };
    state.dirty = true;
}

/// The last [`LOG_READ_CAP`] bytes of the daemon log, the lines that name
/// this server, tail-capped at [`LOG_LINE_CAP`]. The log format is
/// `ts LEVEL target: message` — a server owns `pacode_mcp::*` lines that
/// either quote it (`pool`: `MCP server 'name' …`) or bracket-tag it
/// (`transport::stdio`: `[MCP name] …`, `[MCP stderr name] …`).
fn read_server_log(path: &Path, name: &str) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return vec![format!("daemon log not found: {}", path.display())];
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(LOG_READ_CAP);
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut buf).is_err() {
        return vec!["could not read the daemon log".to_string()];
    }
    let text = String::from_utf8_lossy(&buf);
    // The window may open mid-line — drop the partial first line.
    let text = if start > 0 {
        text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        text.as_ref()
    };
    let quoted = format!("'{name}'");
    let tagged = format!("[MCP {name}]");
    let stderr = format!("[MCP stderr {name}]");
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| {
            l.contains("pacode_mcp")
                && (l.contains(&quoted) || l.contains(&tagged) || l.contains(&stderr))
        })
        .map(str::to_string)
        .collect();
    if lines.len() > LOG_LINE_CAP {
        lines.drain(..lines.len() - LOG_LINE_CAP);
        lines.insert(0, format!("... older lines cut: last {LOG_LINE_CAP} shown"));
    }
    if lines.is_empty() {
        lines.push(format!("no daemon-log lines for '{name}'"));
        lines.push("server stderr logs at debug level (PACODE_LOG=debug)".to_string());
    }
    lines
}
