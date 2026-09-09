//! The rail (spec §4): header, PLAN, AGENTS (or SESSION when idle), BACKGROUND, anchor.
//!
//! - Header: session title bold, id faint, blank line.
//! - PLAN: `PLAN` faint uppercase with `2/4 · 43%` right-aligned; items `[✓]` green +
//!   dim text, `[•]` accent + text, `[ ]` faint; only the active item with a reported
//!   progress gets a second line `    ▰▰▰▰▰▱▱▱ 60%` (bar 8 cells, accent/faint).
//!   In agent-select mode the zone is one line: `PLAN` + `2/4 · 43% ▸`.
//! - AGENTS: header with `5 ●` green; main first (`● main` bold + `думает · 0.4s` faint)
//!   then cards: `○ name`, `  activity` faint (truncated), `  1m47s · ↓ 9.1k` faint.
//!   Cards that do not fit collapse to one line each, then to `  and N more alt+↓`.
//!   If the zone is under 4 lines: single line `5 agents ▾`. In select mode the
//!   selected card is highlighted (reversed bg + `▸`), others one line each.
//!   Finished agents show `✓` green (or `✗` red) and stay in place.
//! - SESSION (idle): `in 214k  out 12.4k  think 8.1k`, `cache hit 71.2%  r 152k / w 61k`
//!   green, `18 turns · $1.84` (cost accent, omitted without pricing), `ctx 41,208 · 21%
//!   used`, blank, `AGENTS` faint + `5 done` list with `and N more`.
//! - BACKGROUND: header `BACKGROUND` + `2 ◍` violet / `1 ✓` green / `1 ✗` red counters;
//!   rows `◍ cargo test 214/380`, `✓ cargo build 3m02s`, `✗ npm run lint 3 err`; max 4
//!   rows then `and N more`.
//! - Anchor: `~/zed/codeapp` dim + `:master` accent, then `• codeapp 0.1.0-dev`.
