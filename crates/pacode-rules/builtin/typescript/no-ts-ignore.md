---
name: ts-no-ts-ignore
languages: [ts, tsx, mts, cts, js, jsx]
match: '@ts-(?:ignore|nocheck)\b'
scope: any
severity: block
max_per_turn: 2
---
Do not suppress the type checker with `@ts-ignore` / `@ts-nocheck`. Fix the type error. If a suppression is truly unavoidable, use `// @ts-expect-error <reason>` on that single line so it breaks when the error goes away.
