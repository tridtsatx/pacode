---
name: rust-no-dbg
languages: [rs]
match: '\bdbg!\s*\('
scope: code
severity: block
mode: stream
max_per_turn: 2
---
Remove `dbg!(...)`: it prints to stderr (corrupting the terminal UI) and must never be committed. Use `log::debug!` if the information is worth keeping.
