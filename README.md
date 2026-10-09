<div align="center">

<img src="assets/logo.png" alt="pacode" width="420">

**A coding agent that lives in your terminal.** Written in Rust, one binary, no runtime.

[![Apache 2.0](https://img.shields.io/badge/licence-Apache--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-edition%202024-orange)](https://www.rust-lang.org)
[![status](https://img.shields.io/badge/status-work%20in%20progress-yellow)](#status)

</div>

> **Status: work in progress.** It builds, it runs and it is used daily, but the
> API and the config are still moving. Proper documentation and a contributing
> guide are coming shortly; until then this README is the documentation.

A background daemon owns the sessions; the TUI is a thin client that attaches to
it. Close the terminal and the work keeps running — reattach and it is still
there, transcript and all.

<img src="assets/screenshot-agents.png" alt="Plan, subagents and background tasks in the rail" width="100%">

## Why another one

Because the ones that exist are heavy. pacode is built so that the cheap thing is
the default:

| | pacode | a typical Electron/Node agent |
|---|---|---|
| Daemon at rest | **17 MB RSS, 0.0% CPU** | hundreds of MB, always warm |
| Daemon + 3 clients | **25.9 MB PSS** (+4.9 MB per client) | one process per window |
| First TUI frame | **6.5 ms** | seconds |
| Binary | one 43 MB static-ish executable | a runtime plus a node_modules tree |

Those are measured, not aspirational. "Economy" is a rule in this repository, not
a nice-to-have: every cache states its cap, its invalidation and its owner, and
**nothing is allowed to tick while the session is idle.** A scheduled job shows
when it runs (`at 14:05`), not a countdown that would need a timer to redraw it.

## Install

```sh
cargo install pacode
```

Or from source, which is what you want if you intend to change anything:

```sh
git clone https://github.com/tridtsatx/pacode.git
cd pacode
cargo build --release
install -m755 target/release/pacode ~/.local/bin/pacode
```

Rust stable, edition 2024. No containers, no runtime, nothing else to install.

## Use it

```sh
pacode                      # attach to the daemon, or start one
pacode "fix the flaky test" # start with a prompt
pacode -s ses_abc123        # resume a session
pacode run --json "..."     # headless, one turn, events on stdout
```

Configuration lives in `~/.config/pacode/config.toml`; `/config` edits it from
inside the TUI. `/login` signs in to a provider that uses OAuth, and
`pacode login --list` shows what is configured.

A provider can be sent through a proxy:

```toml
[provider]
proxy = "http://corp-proxy:3128"     # default for every provider

[providers.openrouter]
proxy = "socks5h://user:pass@127.0.0.1:1080"  # overrides the default
[providers.ollama]
proxy = "none"                       # direct, ignoring the environment too
```

`http`, `https`, `socks5` and `socks5h` are accepted. Without the key the usual
`HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` environment variables apply, with
`NO_PROXY` honoured; a proxy written in the config is used unconditionally, and
`"none"` means a direct connection even when those variables are set. The same
proxy carries that provider's login and token refresh.

## Declaring a provider

A provider is a TOML entry, not a fork. `dialect` picks the wire protocol and
`base_url` + `path` say where it goes, so a new endpoint is a config edit:

```toml
[providers.gateway]
dialect = "openai_compatible"   # openai_compatible | anthropic | codex | devin | opencode_zen
base_url = "https://gateway.internal/v1"
path = "/models/{model}/generate"   # {model} is filled and URL-escaped per request
api_key_env = "GATEWAY_TOKEN"
default_model = "fast-1"
priority = 10
fallbacks = ["openrouter"]

[[providers.gateway.models]]
id = "fast-1"
display_name = "Fast 1"
efforts = ["low", "high"]
max_output_tokens = 8192

[providers.gateway.compat]
thinking_mode = "auto"            # auto | never | always
omit_max_output_tokens = false
```

Every field is optional and defaults to what pacode already did, so an existing
entry keeps its behaviour untouched. `compat` is a closed, typed set: a typo in
a key is a load error, not a setting silently ignored. A bare model name resolves
against `priority` (higher first, ties alphabetically), and `fallbacks` are
tried in declaration order when a provider can no longer serve a route — never in
a loop. Editing the file while the daemon runs rebuilds only the providers whose
entry actually changed, so an in-flight turn keeps the provider instance it
started with.

A dialect is a protocol; if a deployment speaks something genuinely new it needs
a line in the dialect registry in `pacode-provider`, not a new config key.

## Seeing what the client is doing

`pacode stats` prints the daemon's own counters. The client's frame path is a
different process with a different lifetime, so it is opt-in and lands beside
them when asked for:

```sh
PACODE_FRAME_METRICS=1 pacode     # writes render-metrics.json on exit
pacode stats                      # adds a `render` section
```

The reading separates frame, layout and backend cost, counts content,
animation and full repaints separately, and reports the cadence multiplier the
backpressure scheduler was applying. Nominal periods are never raised: a frame
that costs more than its own period degrades the *animation* clock by steps of
×2 up to ×9 after five consecutive slow frames, and the content clock is never
touched — a keystroke is not a clock. A degraded cadence skips animation frames
rather than slowing the animation, because the phase is read off elapsed time and
not off a count of draws.

## Animations

The client runs two clocks. `ui.animation` picks the cadence — `smooth`
(66 ms), `balanced` (100 ms), `eco` (240 ms) — and short transient animations
(overlay unfold, toast) get a faster tick of their own. Nothing animates in
idle: there is no timer running behind a prompt nobody is typing into.

While the agent is working, a light band sweeps across the two lines that only
exist during a turn — the activity label and a running tool's status:

```toml
[ui]
animation = "smooth"   # smooth | balanced | eco
shimmer = "classic"    # classic | kitt | off
```

`classic` is a cosine band crossing the text and wrapping; `kitt` is a Knight
Rider scanner, one bright head ping-ponging with a trail behind it and nothing
lit ahead of it. `off` costs nothing at all — no band, no per-character walk —
and a terminal with no colour (`NO_COLOR`, `ui.color = mono`) is treated as
`off` for the same reason.

The band is a pure function of elapsed animation time, and it advances at most
one cell per painted frame. That is why it takes its speed from the clock's
current period rather than a constant: when backpressure has degraded the
cadence, the same band is painted more sparsely instead of jumping several cells
at once. A sweep that teleports reads as a glitch, not as motion.

## What it does

**Sessions that outlive the terminal.** The daemon holds the session, the client
attaches. Nine numbered slots, `ctrl+alt+1`…`ctrl+alt+9`, switch between
conversations over one connection; the inactive ones drop their heavy structures
and cost about 64 bytes each.

**Subagents.** Spawn them, watch them in the rail, select one and its conversation
takes over the column (`ui.agent_view = "panel"` splits instead). They report back
as an injection between steps, never mid-stream.

**Background commands.** A foreground command that outlives five seconds moves to
the background on its own and keeps going; the rail tracks it, and the model is
told when it ends. Long jobs are reported once, in the transcript, and only when
they ran long enough to be worth the interruption.

**Scheduled work.** `cron` runs a prompt on a schedule — `every 30m` or a
five-field cron expression evaluated in UTC — and survives a daemon restart.
`monitor` watches one condition (a command exiting 0, a process gone, a file
appearing or matching) and tells the model once it comes true, so a turn never
blocks on a wait.

**Questions.** The model can put a decision to you with real options, one marked
as recommended, and wait for the answer the way a permission prompt waits.

<img src="assets/screenshot-question.png" alt="The model asking which platform to build for" width="100%">

**Plugins.** A marketplace in the Claude Code layout, so plugins written for it
work here unchanged: `/plugins` browses, installs and removes them. Skills and MCP
servers from an installed plugin are wired up; what pacode will not run is said
plainly rather than dropped in silence.

**Permissions.** Four modes — build, auto, plan, bypass — over a matrix keyed on
what a tool actually does. Classification fails *closed*: a tool pacode cannot
classify asks, it does not assume.

**MCP.** Servers from your config, plus the ones plugins bring. `pacode import`
picks up servers and skills you already have from Claude Code, Codex, OpenCode,
Cursor, Gemini CLI and VS Code.

**ACP.** `pacode acp` runs it as an Agent Client Protocol agent over stdio, for
editors that speak it.

## In the TUI

| | |
|---|---|
| `alt+↑` / `alt+↓` | move through agents and tasks |
| `alt+1`…`alt+9` | select an agent |
| `ctrl+alt+1`…`ctrl+alt+9` | switch session slot |
| `alt+b` | follow the selected agent |
| `alt+f` | files this turn touched |
| `alt+r` | plan and agents (the only way to reach them under 80 columns) |
| `.` | background tasks |
| `ctrl+v` | paste — an image when the clipboard holds one, text otherwise |
| click a tool call | expand its whole output; click again to fold |
| `shift+tab` | cycle permission mode |
| `ctrl+c` twice | quit |

`/model` (and `/login`) open the model palette: type to fuzzy-search models
and providers, `↑↓` to move, `tab` to step the effort the highlighted model
accepts, `enter` to switch — or to sign in to a provider that needs it.
Chords there: `alt+f` favourite, `alt+z` ZDR-capable, `alt+k` capability
filter, `ctrl+r` refresh the catalog, `alt+a` next account, `alt+x` twice sign
out. Enter on a provider header signs in (another account when signed in).

`/model`, `/effort`, `/mode`, `/theme`, `/config`, `/sessions`, `/mcp`,
`/plugins`, `/keys`, `/editor`, `/import`, `/compact`, `/clear` (reset the
conversation in place), `/new` (a fresh session), `/export`, `/help`.

Every binding is rebindable from `/keys`.

## Layout

Nineteen crates, each with one job:

| | |
|---|---|
| `pacode-types` | wire types and pure logic — no IO, no tokio, few dependencies |
| `pacode-core` | the turn loop, sessions, agents, permissions, scheduling |
| `pacode-daemon` | the server: sockets, connections, lifecycle |
| `pacode-tui` / `pacode-render` / `pacode-client` | the terminal client |
| `pacode-provider` | model providers and the catalog |
| `pacode-tools` | the built-in tool set |
| `pacode-exec` | supervised processes, output spooling, progress parsing |
| `pacode-store` | SQLite on a worker thread, with FTS5 search |
| `pacode-mcp` / `pacode-plugin` / `pacode-skills` / `pacode-acp` | the edges |

House rules: modules under 500 lines, exhaustive matches, no `unwrap()` outside
tests, `thiserror` in public APIs, and a clean `cargo clippy --workspace
--all-targets` on every commit.

## Status

Work in progress, and honest about it:

- **Works today:** sessions, subagents, permissions, background commands, cron
  jobs and monitors, questions, MCP, plugins and the marketplace, skills, ACP,
  import from other agents.
- **Coming shortly:** proper documentation, a contributing guide, and the
  remaining tabs of the plugin screen.
- **Not there yet:** Windows (Unix sockets and process groups are assumed), and
  running Claude Code hooks or markdown slash commands from a plugin.

Issues and pull requests are welcome meanwhile; the checks below are what CI will
run.

## Contributing

```sh
cargo fmt
cargo clippy --workspace --all-targets   # must be silent
cargo test --workspace
```

Tests live next to what they test, in `*_tests.rs`. Anything that reaches the
model is size-capped; anything that caches states its cap, its invalidation and
its owner. If a fix needs a hack, say so in the code with the condition for
removing it.

## Prior art

pacode is not the first terminal coding agent and does not pretend to be. It was
built while reading, and borrowing ideas from:

- [Claude Code](https://www.anthropic.com/claude-code) — the shape of the whole
  thing: the turn loop, permission modes, subagents, skills, and the plugin
  format this marketplace speaks. Not open source; the influence is on the
  design, not the code.
- [OpenCode](https://github.com/sst/opencode) — a daemon a thin client attaches
  to, and sessions that outlive the terminal.
- [Codex](https://github.com/openai/codex) — supervised process execution and
  output handling; the head/tail output buffer here is a port of its idea.
- [jcode](https://github.com/1jehuang/jcode) — the most RAM-efficient harness,
  and the reason this one measures itself at all; the rail layout and much of the
  TUI vocabulary came from reading it.

## License

Apache 2.0. See [LICENSE](LICENSE).
