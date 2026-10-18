---
name: rust-no-unwrap-expect
languages: [rs]
match: '\.(?:unwrap|expect)\('
scope: code
severity: warn
max_per_turn: 3
exclude: ['*_tests.rs', 'tests/', 'benches/', 'examples/', 'build.rs', 'test_support.rs']
skip_test_code: true
---
Do not use `.unwrap()` / `.expect(...)` in non-test code. Propagate with `?` (add a thiserror variant if needed), handle the `None`/`Err` case explicitly, or use `unwrap_or_else` with a sensible fallback. Tests may unwrap.
