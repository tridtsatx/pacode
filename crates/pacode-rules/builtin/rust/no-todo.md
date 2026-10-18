---
name: rust-no-todo
languages: [rs]
match: '\b(?:todo|unimplemented)!'
scope: code
severity: warn
max_per_turn: 2
skip_test_code: true
exclude: ['*_tests.rs', 'tests/']
---
Do not leave `todo!()` / `unimplemented!()` in the code: they panic at runtime. Implement the behaviour now, or return a proper error variant.
