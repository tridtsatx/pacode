---
name: ts-no-explicit-any
languages: [ts, tsx, mts, cts]
match: '(?::\s*any\b|\bas\s+any\b|[<,]\s*any\s*[>,\[]|\bany\s*\[\s*\]|\|\s*any\b)'
scope: code
severity: block
mode: stream
max_per_turn: 3
exclude: ['*.d.ts']
---
Never write `any`; use `unknown` (and narrow it) or an explicit type. For untyped JSON use a `type`/`interface` plus a type guard; for generics use a constrained type parameter (`<T extends ...>`).
