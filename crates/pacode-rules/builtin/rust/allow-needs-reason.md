---
name: rust-allow-needs-reason
languages: [rs]
match: '#!?\[allow\(\s*clippy::[A-Za-z0-9_:]+(?:\s*,\s*clippy::[A-Za-z0-9_:]+)*\s*,?\s*\)\]'
unless: '\)\]\s*//\s*\S'
scope: code
severity: warn
max_per_turn: 2
---
`#[allow(clippy::...)]` needs a reason. Fix the lint instead, or write `#[allow(clippy::name, reason = "why this is fine")]` (or a trailing `// why` comment).
