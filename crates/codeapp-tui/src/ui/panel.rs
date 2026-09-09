//! Agent / task panel inside the dialog column (mockup states 03, 04, 05).
//!
//! Header: `▸ name · 41s` (or `FOLLOW` badge + name when following, `paused` when
//! follow is paused), second line `Edit sidebar.rs · ↓ 9.1k` faint, blank, then the
//! agent transcript via `dialog` rendering, autoscrolled when following. Task panel:
//! header `◍ cargo test · 1m47s · 214/380`, then raw output lines (no markdown), tail
//! follows while running. A finished agent shows its summary at the end.
