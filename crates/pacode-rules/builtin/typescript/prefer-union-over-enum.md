---
name: ts-prefer-union-over-enum
languages: [ts, tsx, mts, cts]
match: '^\s*(?:export\s+)?(?:declare\s+)?(?:const\s+)?enum\s+[A-Za-z_$]'
scope: code
severity: warn
max_per_turn: 1
exclude: ['*.d.ts']
---
Prefer a string-literal union (`type Mode = 'a' | 'b'`) or an `as const` object over `enum`: unions are erased at runtime, tree-shake, and work with isolatedModules/erasable syntax.
