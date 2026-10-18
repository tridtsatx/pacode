---
name: ts-strict-equality
languages: [ts, tsx, mts, cts, js, jsx]
match: '(?:^|[^=!<>])==(?:[^=]|$)|!=(?:[^=]|$)'
unless: '^.?\s*(?:==|!=)\s*null\b'
scope: code
severity: warn
max_per_turn: 2
---
Use `===` / `!==` instead of `==` / `!=` (no implicit coercion). The only accepted loose comparison is `== null` / `!= null` for null-or-undefined checks.
