//! Footer rows (spec §3) and width degradation (spec §8).
//!
//! Row 1: `Build` cyan `·` faint, model display name dim, ` | ` faint, effort accent,
//! ` /effort` cyan when `ui.hints.effort`, `/model` cyan right-aligned when
//! `ui.hints.model`.
//! Row 2 (Normal focus): `▸▸ <mode.permission_line()> (shift+tab to cycle)` (red for
//! Bypass, cyan otherwise, hint faint) `· ← 5 agents · 1 bg` faint; right: `10.7K ·
//! ctrl+p` faint. Idle: `· 5 agents done · 22m40s`.
//! Row 2 in other focuses replaces the left part with contextual hints:
//! SelectAgent `▸▸ <name> 4/5 · enter открыть · alt+b follow · esc снять`;
//! Panel `▸▸ <name> · esc назад · alt+b follow · s стоп`;
//! Follow `FOLLOW` badge + `<name> 18m41s · pgup пауза · alt+b отпустить`;
//! BgList `▸▸ 2 bg running · 1 failed · enter вывод · k убить · esc`.
//! Reconnecting: row 2 left becomes `reconnecting… (attempt N)` red.
//! Compression order when narrow: drop hints, then bg counter, then agents counter,
//! then context, then permission text; model+effort always stay. Right part keeps ≥ 2
//! spaces from the left part.
