---
name: rust-no-println
languages: [rs]
match: '\b(?:println|eprintln|print|eprint)!\s*\('
scope: code
severity: warn
max_per_turn: 2
skip_test_code: true
exclude: ['main.rs', 'bin/', 'build.rs', 'examples/', 'benches/', 'tests/', '*_tests.rs']
---
Library code must not print to stdout/stderr (it corrupts the terminal UI and the JSON protocols). Use `log::info!/warn!/debug!`, or return the text to the caller.
