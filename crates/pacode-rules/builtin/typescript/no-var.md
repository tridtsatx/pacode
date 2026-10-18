---
name: ts-no-var
languages: [ts, tsx, mts, cts, js, jsx]
match: '\bvar\s+[A-Za-z_$\[{]'
scope: code
severity: warn
max_per_turn: 2
---
Do not use `var`; it is function-scoped and hoisted. Use `const` (default) or `let` when the binding is reassigned.
