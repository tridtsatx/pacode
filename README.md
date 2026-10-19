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
| Binary | one ~60 MB executable (release build, 2026-10-19; it was 43 MB before the latest features) | a runtime plus a node_modules tree |

Those are measured, not aspirational; the daemon, PSS and first-frame figures date
from before the binary grew and have not been re-measured since. "Economy" is a rule in this repository, not
a nice-to-have: every cache states its cap, its invalidation and its owner, and
**nothing is allowed to tick while the session is idle.** A scheduled job a day
away costs one wakeup a minute to redraw its countdown, not sixty.

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

To build only the binary, name the package: `cargo build --release -p pacode`.
Building the whole workspace unifies Cargo features across crates and can enable
more than the binary asks for. By default `read` can outline Rust, TypeScript,
JavaScript, Python, Go, Java, C, Bash, JSON, TOML, YAML and Markdown; for C++, C#,
Kotlin, Ruby, PHP and Swift add `--features outline-extra` (a larger binary).

## Use it

```sh
pacode                      # attach to the daemon, or start one
pacode "fix the flaky test" # start with a prompt
pacode -s ses_abc123        # resume a session
pacode run --json "..."     # headless, one turn, events on stdout
```

Configuration lives in `~/.config/pacode/config.toml`; `/config` edits it from
inside the TUI. `/login` signs in to a provider (OAuth or API key), and
`pacode login --list` shows what is configured.

### Providers

Every provider below works with no `config.toml` entry: export its key, or run
`/login <id>` and paste one. A `[providers.<id>]` table with the same id replaces the
builtin.

| id | Provider | Base URL | Key |
|---|---|---|---|
| `openai` | OpenAI, ChatGPT subscription (Codex OAuth) | `https://chatgpt.com/backend-api/codex` | `/login openai` |
| `openai-api` | OpenAI API | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| `openrouter` | OpenRouter | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| `moonshot` | Kimi (Moonshot) | `https://api.moonshot.ai/v1` | `MOONSHOT_API_KEY` |
| `kimi-code` | Kimi Code coding plan | `https://api.kimi.com/coding/v1` | `KIMI_API_KEY` |
| `zai` | Z.AI (GLM) | `https://api.z.ai/api/paas/v4` | `ZAI_API_KEY` |
| `zai-coding` | Z.AI coding plan | `https://api.z.ai/api/coding/paas/v4` | `ZAI_API_KEY` |
| `opencode-zen` | OpenCode Zen | `https://opencode.ai/zen/v1` | `OPENCODE_API_KEY` |
| `opencode-go` | OpenCode Go | `https://opencode.ai/zen/go/v1` | `OPENCODE_API_KEY` |
| `gemini` | Google Gemini (OpenAI-compatible endpoint) | `https://generativelanguage.googleapis.com/v1beta/openai` | `GEMINI_API_KEY` |
| `ollama`, `lmstudio` | local servers | `http://localhost:11434/v1`, `http://localhost:1234/v1` | none |

Any other OpenAI-compatible server is a custom provider:

```toml
[providers.vllm]
base_url = "http://gpu-box:8000/v1"
api_key_env = "VLLM_API_KEY"
catalog = true                       # list models from GET /models
```

Endpoint differences (`max_completion_tokens` on OpenAI, `reasoning` object on
OpenRouter, binary `thinking` on Z.AI and Kimi K2, tool-id limits, Gemini schema
limits) are applied by host, so a custom entry pointing at one of these hosts gets
them too. Devin keeps its own login (`/login devin`).

Anthropic is not a provider: the `anthropic` kind was removed, and a config entry
with `kind = "anthropic"` fails with an explicit error instead of being guessed at.

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

## What it does

**Sessions that outlive the terminal.** The daemon holds the session, the client
attaches. Nine numbered slots, `ctrl+alt+1`…`ctrl+alt+9`, switch between
conversations over one connection; the inactive ones drop their heavy structures
and cost about 64 bytes each.

**Subagents.** Spawn them, watch them in the rail, select one and its conversation
takes over the column (`ui.agent_view = "panel"` splits instead). They report back
as an injection between steps, never mid-stream. The orchestrator talks to a
running one through the `agent` tool: `message` sends an instruction or
correction, `answer` replies to a question, `result` fetches the final answer if
the delivered report was cut short. A subagent that is blocked can put a question
to its orchestrator with `ask_parent`; `agents.ask_parent_timeout_secs` and
`agents.ask_parent_max` bound the wait and the number of questions. Named subagent
kinds come from `agents/*.md` files.

**Agents do not clobber each other.** Main agent and subagents write one working
tree, with no worktrees. A file ownership ledger (in memory, per session) tracks
who last wrote each path and what each agent has read. Writing an existing file you
never read is rejected (`read X first`). Writing over lines another agent changed
since your read is rejected with who changed which lines and the fresh anchored
text, which counts as read, so one retry is enough; changes elsewhere in the file
only produce a note. Writers of one path take a per-path lock. The ledger is lost
on daemon restart, so read-first applies again.

**Hashline edits.** `read` returns rows as `LINE#HASH| text`. `edit` takes ops that
name those anchors instead of retyping unchanged text, and applies all of them or
none: a stale anchor rejects the edit with fresh anchored context, and an anchor
whose line drifted is relocated when exactly one line nearby carries its hash. The
old `old_string` / `new_string` form is still accepted.

**Smart read.** A source file longer than `read.outline_threshold` lines (default
400, `0` turns it off) comes back as a tree-sitter outline, signatures kept and
bodies elided, when that is clearly shorter. `mode` is `auto`, `full` or
`outline`; `symbol` returns one declaration in full (`"bar"`, `"Foo::bar"`);
`expand` returns line ranges from the outline's markers. `offset` / `limit` always
read plainly.

**Style guard.** Rules are markdown files with a small frontmatter (`name`,
`languages`, `match` regex run on each *added* line, optional `unless`, `scope`,
`severity` = `warn` / `block`, `mode` = `after_write` / `stream`, `max_per_turn`,
`exclude`) in `<project>/.pacode/rules/*.md` or `<config dir>/rules/*.md`. A hit is
appended to the tool result with the rule's text; a `stream` rule can cut a
write/edit call short while its arguments are still streaming. Built-in packs
cover Rust (no `unwrap`/`expect`, `dbg!`, `println!`, lossy casts, `unsafe` without
a `SAFETY` comment, and a few more) and TypeScript (no `any`, `var`,
`@ts-ignore`, `console.log`, `==`, enums). Later layers override earlier ones by
name: built-in, then global, then project.

```toml
[rules]
enabled = true
builtin = true               # the Rust and TypeScript packs
disable = ["rust-no-println"]  # rule names removed from every layer
stream = true                # allow stream rules to cut a call short
```

**Memory.** A global store (`<config dir>/memory.db`) and a per-project one
(`<project>/.pacode/memory.db`; add it to `.gitignore`), both SQLite with FTS5.
`memory_write` (near-duplicates update the old entry), `memory_recall` (ranked by
text match, recency, pinning and recall count), `memory_list` and `memory_forget`
(soft delete, purged after 90 days) are tools; pinned entries and a short index go
into the system prompt, capped by `context.memory_cap_chars`. `[memory]` has
`enabled` (default on) and `embeddings`, which does nothing yet: no embedder ships.

**Built-in coreutils.** The `util` tool runs common text utilities in-process, with
no shell and no fork: `wc head tail sort uniq cut tr nl tac rev paste join basename
dirname realpath stat du tree diff jq sha256sum md5sum base64 xxd sed`. They are
read-only and a flag a utility does not implement is an error, never ignored. A
`bash` command that is a plain pipeline of these (no redirection, globs or
expansion) takes the same fast path and falls back to the real shell on any
failure, so error text comes from the real tools.

**Images.** `ctrl+v` pastes an image as a chip in the prompt; it is removed whole.
Images in the transcript are drawn inline as half-block cells, so they scroll and
redraw like text (`ui.images = "off"` shows a one-line stub instead). Sending an
image to a model that does not report vision support is refused with a message
rather than silently dropped.

**Todo guard.** If the model ends its turn while the plan still has open items, a
reminder is injected and it continues. `agents.todo_guard` (default on) and
`agents.todo_guard_max_reminders` bound it per user turn; only the main agent is
guarded.

**Compaction.** Triggered at `context.compaction_threshold` of the window, aimed at
`context.compaction_target_ratio`. First a mechanical shake replaces old large tool
results with short stubs (the last `context.keep_recent_turns` turns and the newest
result per file are untouched). Then a structured summary of the older part (goal,
decisions, files touched, commands, open problems, facts), written by
`context.compaction_model` when set. Then pinned blocks (plan, tasks, a file-ledger
digest) are re-injected verbatim. If the summary call fails it degrades to the shake
plus a deterministic digest instead of an error.

**Instruction files.** `AGENTS.md`, `PACODE.md` and `CLAUDE.md` are read from the
project and its ancestors, and `AGENTS.md` / `PACODE.md` from the config dir. With `context.discover_foreign_instructions` (default on) the files
written for other agents are picked up too: `.cursor/rules`, `.clinerules`,
Copilot, Windsurf, Gemini and Codex files in the project and its ancestors.

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
| `shift+tab` / `alt+m` | cycle permission mode (`alt+m` for terminals that swallow `shift+tab`) |
| `ctrl+c` twice | quit |

`/model`, `/effort`, `/mode`, `/theme`, `/config`, `/sessions`, `/mcp`,
`/plugins`, `/keys`, `/editor`, `/import`, `/compact`, `/export`, `/login`,
`/clear`, `/help`, `/quit`.

Every binding is rebindable from `/keys`.

**Model picker.** `/model` lists providers on the left and their models on the
right, with context window, a detail line for the highlighted model and a search
box. `alt+←` / `alt+→` filters by kind (chat, image, embedding, other). `r` picks
which role the next choice applies to: `default` (the session's model), `agents`
(`agents.default_model` and `agents.default_effort`) or `compaction`
(`context.compaction_model`). Effort is limited to the levels the chosen model
actually supports.

## Resource limits

Agents run arbitrary commands, so pacode keeps the machine usable. By default 20% of
memory stays free (of the cgroup limit when one is lower than the host's RAM). Below
that reserve, new commands and subagents are refused with `low resources: ...` until
running ones finish. Stopping is soft: only a shortfall of more than `overshoot_mb`
beyond the reserve that lasts `grace_secs` counts, and then only the largest process
tree (never the daemon, the TUI or MCP servers, never one younger than 2 s) gets
SIGTERM, and SIGKILL after `term_wait_secs`. The task reports why it was stopped so
the agent can retry with less memory. Sampling runs only while a command is running.
Spawned commands run at lower CPU priority and output spooling stops when the spool
disk is nearly full. Linux is fully supported; macOS is best effort.

```toml
[resources]
enabled = true
min_free_percent = 20      # of total memory (or cgroup limit)
overshoot_mb = 512         # tolerated beyond the reserve before it is a breach
grace_secs = 10            # a breach must last this long
term_wait_secs = 5         # SIGTERM, wait, then SIGKILL
max_process_percent = 0    # optional cap for one process tree, 0 = off
nice = 10                  # CPU niceness of spawned commands, 0 = off
min_free_disk_mb = 512     # spool disk reserve
sample_ms_idle_headroom = 1000
sample_ms_pressure = 200
```

## Many agents at once

Subagents multiply load, so the daemon limits it in four places:

- **Provider gate.** `provider.max_concurrent` (default 64, `0` = unlimited) is a
  ceiling on in-flight requests per provider across all agents. The effective limit
  adapts: halved on each 429/503 episode, raised by one after a run of successes,
  and a `Retry-After` pauses every request to that provider instead of each agent
  retrying alone.
- **Exec admission queue.** When `exec.max_tasks` processes are already running,
  a new command waits in FIFO order for a slot instead of failing.
- **Interest subscriptions.** A client says which agents' high-volume events
  (text and reasoning deltas, transcript items) it wants; the daemon drops or
  coalesces the rest, so a TUI drawing one agent does not receive 256.
- **Idle session eviction.** A session with no running turn, agent, task, monitor,
  subscriber or imminent cron job, untouched for ten minutes, is unloaded; resuming
  rebuilds it from the store. Only in-memory state is lost (ledger, live monitors).

`Request::DebugStats` returns the daemon's counters as JSON for diagnostics.

Measured with a load bench: 256 simulated streaming subagents against a local mock
provider on a 4-core machine. Text latency p99 was 28 to 36 ms, the daemon used
roughly 0.03 to 0.1 core, RSS peaked at about 120 to 240 MB and was 47 to 84 MB at
the end of the run, no events were lost, no turns failed, and spawning 256 agents
took about 0.5 s. This is a mock provider, not real model latency. Two of the bench's
own checks still fail: memory retained per finished agent (about 100 KB each, so RSS
grows from 47 to 67 MB over four waves of 64 agents) and its idle "wakeups per minute"
limit, which counts context switches rather than timers (the daemon's only idle timer
is a 30 s poll, and idle CPU is zero ticks).

## Layout

Twenty-five crates, each with one job:

| | |
|---|---|
| `pacode` | the binary: CLI, daemon launcher, `acp`, `import`, `login` |
| `pacode-types` | wire types and pure logic — no IO, no tokio, few dependencies |
| `pacode-config` | XDG paths, TOML loading, env overrides, the settings registry behind `/config` |
| `pacode-core` | the turn loop, sessions, agents, permissions, compaction, scheduling, the file ledger |
| `pacode-daemon` | the server: sockets, connections, event fan-out, lifecycle |
| `pacode-tui` / `pacode-render` / `pacode-client` | the terminal client |
| `pacode-image` | image decoding, protocol detection and half-block rendering |
| `pacode-provider` | model providers, the catalog, the provider gate |
| `pacode-auth` / `pacode-net` | OAuth and the credential store; proxy configuration |
| `pacode-tools` | the built-in tool set, including hashline edits |
| `pacode-coreutils` | in-process `wc`, `sort`, `sed`, `jq`, `tree`, `diff` and friends |
| `pacode-outline` | tree-sitter outlines for smart `read`, one feature per grammar |
| `pacode-rules` | style-guard rule files, the built-in Rust and TypeScript packs, the scanner |
| `pacode-memory` | global and per-project agent memory, SQLite with FTS5 recall |
| `pacode-risk` | deterministic shell-command risk classification |
| `pacode-exec` | supervised processes, output spooling, progress parsing, admission queue |
| `pacode-store` | SQLite on a worker thread, with FTS5 search |
| `pacode-mcp` / `pacode-plugin` / `pacode-skills` / `pacode-acp` / `pacode-import` | the edges |

House rules: modules under 500 lines, exhaustive matches, no `unwrap()` outside
tests, `thiserror` in public APIs, and a clean `cargo clippy --workspace
--all-targets` on every commit.

## Status

Work in progress, and honest about it:

- **Works today:** sessions, subagents and their messaging, permissions, background
  commands, cron jobs and monitors, questions, hashline edits, the file ledger,
  smart read, style-guard rules, memory, built-in coreutils, images, the model
  picker, compaction, MCP, plugins and the marketplace (skills and MCP servers),
  skills, ACP, import from other agents. Linux is the supported platform; macOS is
  best effort.
- **Coming shortly:** proper documentation, a contributing guide, and the
  remaining tabs of the plugin screen.
- **Not there yet:** Windows (Unix sockets and process groups are assumed), running
  Claude Code plugin hooks or markdown slash commands from a plugin, and semantic
  (embedding) memory recall. Shell hooks you write yourself in `[hooks]` in
  `config.toml` are a separate feature and do work.

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
- [oh-my-pi](https://github.com/can1357/oh-my-pi) — hashline edits (anchored
  `LINE#HASH` lines instead of retyped text), the smart-read ideas (outlines,
  reading a symbol or a range), style-guard rules that act when a model writes
  code (their TTSR), the todo guard and the shape of the model picker.
- [jcode](https://github.com/1jehuang/jcode) — the most RAM-efficient harness,
  and the reason this one measures itself at all; the rail layout and much of the
  TUI vocabulary came from reading it.

## License

Apache 2.0. See [LICENSE](LICENSE).
